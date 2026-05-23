//! Detects function arguments and replaces their reads with canonical
//! [`NodeKind::FunctionArg`] nodes.
//!
//! Runs as a post-pass after the main fixed-point loop converges.  Rewrites
//! register-passed arg reads (`InitialVar(arg_reg)`) and stack-passed arg
//! reads (`Load[InitialVar(sp) + K]` unshadowed by any prior store) into a
//! single canonical form keyed by the argument's index in the calling
//! convention.
//!
//! # Detection rules
//!
//! * **Register args** (no contiguity constraint).  For each register
//!   `R = cc.arg_passing_regs[i]`, if `InitialVar(R)` has live uses in the
//!   graph, emit one `FunctionArg { Register(R), i }` and rewire every use
//!   of `InitialVar(R)`'s output to point at the new node.
//!
//! * **Stack args** (strict contiguity + no-shadow).  Collect all `Load`
//!   nodes whose address decomposes (via [`sp_expr::decompose_sp`]) to
//!   `InitialVar(sp) + K` with `K == cc.stack_arg_offsets[j]`.  Reject any
//!   whose memory input is reachable backward from a shadowing store — the
//!   walk is a DFS through memory predecessors that treats `MemPhi` as a
//!   fork where every predecessor must be non-disqualifying.  Disqualifying
//!   nodes: `StackStore { offset: K }`, `StackStorePhi` whose per-predecessor
//!   offsets contain `K`, and un-decomposed `Store` (may alias —
//!   conservative).  Non-disqualifying: `InitialMemory`, `Call`,
//!   `CallOther`, and stores at other offsets.  After
//!   filtering, emit only those indices that form a gap-free prefix starting
//!   at `first_stack_arg = arg_passing_regs.len()`; the first gap truncates.
//!
//! Each emitted `FunctionArg` has an output width equal to the widest load
//! observed at that offset (register sources use the container register's
//! natural width); narrower reads are rewired through `Truncate`.

use strider_ir::node::{FunctionArgSource, NodeId, NodeKind, NodeOutputId, NodeOutputKind, NodeOutputType};

use crate::opt::error::Result;
use crate::opt::mem_walk::{CyclePolicy, MemChainStep, StepResult, walk_mem_chain};
use crate::opt::pipeline::{OptimizationResult, Optimizer};
use crate::opt::sp_expr::{
    AliasStep, SpExpr, SpExprMemo, decompose_sp, ranges_disjoint, step_through_stack_store,
    step_through_stack_store_phi, step_through_store,
};
use crate::opt::worklist::seeded_kind;

/// Replaces register-passed and stack-passed argument reads with canonical
/// [`NodeKind::FunctionArg`][strider_ir::node::NodeKind::FunctionArg] nodes.  Intended
/// to run once, as an
/// [`OptimizerPipeline::add_post_pass`][crate::opt::OptimizerPipeline::add_post_pass]
/// after the fixed-point loop has converged.
#[derive(Clone)]
pub struct FunctionArgDetect {
    /// Calling convention this pass was built from.  See the comment
    /// on `StackStoreDetect::cc` for the per-pass-shared-Arc rationale.
    /// Consults `cc.arg_passing_regs`, `cc.stack_ptr_vn`, and
    /// `cc.stack_arg_offsets` indirectly through [`Self::layout`].
    cc: std::sync::Arc<strider_target::BuiltCallingConvention>,
    /// Cached positional-arg layout derived from `cc` at construction
    /// time.  The pass reads `layout.first_stack_index()` to compute
    /// the register-vs-stack boundary instead of the
    /// `arg_passing_regs.len()` derivation that used to live inline
    /// here — single source of truth for "what is positional arg `i`?".
    layout: strider_target::PositionalArgLayout,
}

impl FunctionArgDetect {
    /// Creates a new pass with an explicit register list, stack-pointer
    /// varnode, and stack-arg offset table.  Convenience constructor;
    /// production paths prefer [`Self::from_convention`].
    #[must_use]
    pub fn new(
        arg_passing_regs: Vec<rsleigh::Vn>,
        stack_ptr_vn: rsleigh::Vn,
        stack_arg_offsets: Vec<i64>,
    ) -> Self {
        let cc = crate::opt::sp_pass_cc::minimal_cc(stack_ptr_vn, arg_passing_regs, stack_arg_offsets);
        Self::from_convention(&cc)
    }

    /// Creates a new pass whose parameters are taken from the supplied
    /// calling convention.
    #[must_use]
    pub fn from_convention(cc: &strider_target::BuiltCallingConvention) -> Self {
        let layout = strider_target::PositionalArgLayout::from_convention(cc);
        Self {
            cc: std::sync::Arc::new(cc.clone()),
            layout,
        }
    }

    /// Convention this pass was built with.
    #[must_use]
    pub fn calling_convention(&self) -> &strider_target::BuiltCallingConvention {
        &self.cc
    }
}

impl Optimizer for FunctionArgDetect {
    fn optimize(
        &self,
        graph: &mut strider_ir::Graph,
        entry: NodeId,
    ) -> Result<OptimizationResult> {
        let mut ctx = crate::pattern::RewriteCtx::new(graph, entry);
        let mut changed = OptimizationResult::NoChange;
        // `layout.register_args()` yields slots in ABI order, with
        // canonical positional indices stamped at layout-construction
        // time.  `layout.first_stack_index()` replaces the local
        // `arg_passing_regs.len()` derivation that used to live here.
        let arg_passing_regs: Vec<rsleigh::Vn> =
            self.layout.register_args().map(|(_, vn)| vn).collect();
        let stack_arg_offsets: Vec<i64> =
            self.layout.stack_args().map(|(_, o)| o).collect();
        changed |= detect_register_args(&mut ctx, &arg_passing_regs)?;
        changed |= detect_stack_args(
            &mut ctx,
            self.cc.stack_ptr_vn,
            &stack_arg_offsets,
            self.layout.first_stack_index() as usize,
        )?;
        // Replacing `Load[sp+K]` with `FunctionArg` orphans the address-
        // computation chain (`Add(sp, K)` and friends).  Those nodes remain in
        // the use-list of surviving producers like `InitialVar(sp)`, which
        // confuses downstream consumers that walk use-lists — e.g. the dot
        // renderer draws an edgeless `InitialVar(sp)` island.  Detach them.
        // The detach result is hygiene-only (post-pass return values are
        // ignored by the pipeline); don't escalate it into `Changed`.
        let entry = ctx.entry();
        let _ = crate::opt::worklist::detach_unreachable_nodes(ctx.graph_mut(), entry);
        Ok(changed)
    }
}

/// Rule D: for every register in `arg_passing_regs` whose `InitialVar` node
/// has live uses, emit one `FunctionArg { Register(reg), i }` and rewire all
/// those uses to it.  No contiguity check — reading only arg 2 still labels it
/// arg 2.
///
/// **Sub-register fallback.**  The IR builder doesn't always promote a
/// register read at function entry to the full container register: a `char`
/// or `int` parameter compiled on x86_64 SysV may surface as
/// `InitialVar(ECX size=4 at off=8)` rather than `InitialVar(RCX size=8
/// at off=8)`, and on AArch64-BE a 32-bit `int` parameter may surface as
/// `InitialVar(W3 size=4 at off=X3.off+4)` (BE places the 32-bit
/// sub-register in the high half of the 64-bit container).
///
/// When the exact-`Vn` lookup misses, fall back to any `InitialVar` whose
/// `Vn` lies fully within `reg`'s byte range
/// `[reg.addr_off, reg.addr_off + reg.size)` in the same address space.
/// If multiple candidates exist, pick the largest (the most specific
/// reading of `reg`'s state).  The emitted `FunctionArg`'s `source`
/// records the actual sub-register Vn, so downstream consumers see the
/// width the function actually reads.
fn detect_register_args(
    ctx: &mut crate::pattern::RewriteCtx<'_>,
    arg_passing_regs: &[rsleigh::Vn],
) -> Result<OptimizationResult> {
    // Single reachable-graph scan collects every InitialVar's Vn → NodeId.
    // `InitialVar` nodes are not hash-cached (see `NodeKind::is_cacheable`),
    // so we still rely on the builder's invariant of at most one InitialVar
    // per varnode.  Walking `preorder()` rather than `all_node_ids()` skips
    // detached zombies left by destructive passes (e.g. `RedundantPhis`),
    // matching every other pass in this crate.
    let mut initial_vars: rustc_hash::FxHashMap<rsleigh::Vn, NodeId> =
        rustc_hash::FxHashMap::default();
    for n in ctx.preorder() {
        if let NodeKind::InitialVar(vn) = *ctx.node_kind(n) {
            initial_vars.insert(vn, n);
        }
    }

    /// Find the largest `(Vn, NodeId)` whose Vn is fully contained
    /// in `reg`'s byte range.  Returns `None` if nothing's contained.
    fn largest_sub_in(
        initial_vars: &rustc_hash::FxHashMap<rsleigh::Vn, NodeId>,
        reg: rsleigh::Vn,
    ) -> Option<(rsleigh::Vn, NodeId)> {
        let lo = reg.addr_off;
        let hi = reg.addr_off + (reg.size as u64);
        initial_vars
            .iter()
            .filter(|(vn, _)| {
                vn.addr_space == reg.addr_space
                    && vn.addr_off >= lo
                    && vn.addr_off + (vn.size as u64) <= hi
            })
            .max_by_key(|(vn, _)| vn.size)
            .map(|(vn, n)| (*vn, *n))
    }

    let mut result = OptimizationResult::NoChange;
    for (i, reg) in arg_passing_regs.iter().enumerate() {
        // Exact match → use as-is.  Otherwise the largest sub-register
        // contained in `reg`'s byte range.
        let (effective_vn, initial_var) = if let Some(&n) = initial_vars.get(reg) {
            (*reg, n)
        } else if let Some((sub_vn, sub_n)) = largest_sub_in(&initial_vars, *reg) {
            (sub_vn, sub_n)
        } else {
            continue;
        };

        let [old_out] = ctx.node_outputs_exact::<1>(initial_var)?;
        // Skip if the InitialVar has no consumers.
        if ctx.output_use_cursor(old_out).current().is_none() {
            continue;
        }

        let out_type = NodeOutputType::try_from(effective_vn.size)?;
        // Inherit the InitialVar's asm-fingerprint so downstream pattern
        // queries (`m.asm_fingerprint(c, &graph)` on a captured FunctionArg)
        // can still trace back to the contributing machine instruction.
        // FunctionArg is exempt from the validator's non-empty-fingerprint
        // check, but the superset-only contract still says passes may grow
        // fingerprints — never shrink them — when replacing a node's uses.
        // The single-source register-args path (one InitialVar in, one
        // FunctionArg out) carries no coupling concern; the stack-args path
        // unifies multiple Loads and intentionally skips the absorption.
        let new_node = ctx.create_node_attributed(
            NodeKind::FunctionArg {
                source: FunctionArgSource::Register(effective_vn),
                index: i as u32,
            },
            [],
            [NodeOutputKind::OutputType(out_type)],
            &[initial_var],
        );
        let [new_out] = ctx.node_outputs_exact::<1>(new_node)?;
        result |= OptimizationResult::from_changed(ctx.replace_all_uses(old_out, new_out)?);
    }
    Ok(result)
}

/// Rule (stack args): collect every `Load` node whose address decomposes to
/// `InitialVar(sp) + K` where `K` is one of the convention's stack-arg
/// offsets.  Group by `K`, then apply **strict contiguity** from position 0:
/// the first gap in the offset-set truncates, so surviving indices are a
/// gap-free prefix.  For each surviving `K`, emit one `FunctionArg` and rewire
/// every qualifying load's uses to it.
///
/// Memory-shadow disqualification and width merging extend this further.
fn detect_stack_args(
    ctx: &mut crate::pattern::RewriteCtx<'_>,
    sp_vn: rsleigh::Vn,
    stack_arg_offsets: &[i64],
    first_stack_arg: usize,
) -> Result<OptimizationResult> {
    if stack_arg_offsets.is_empty() {
        return Ok(OptimizationResult::NoChange);
    }

    // Group candidate loads by their position `j` in `stack_arg_offsets`.
    // A load qualifies only if (a) its address decomposes to `sp + K` where
    // `K` is a convention offset, and (b) nothing on its memory chain may
    // alias that slot (DFS shadow check).  If *any* load at offset K is
    // shadowed, the whole K-group is disqualified (conservative).
    let mut memo: SpExprMemo = SpExprMemo::default();
    let mut shadow_memo: ShadowMemo = ShadowMemo::default();
    let mut groups: rustc_hash::FxHashMap<usize, Vec<NodeId>> =
        rustc_hash::FxHashMap::default();
    let mut disqualified: rustc_hash::FxHashSet<usize> = rustc_hash::FxHashSet::default();
    let mut work = seeded_kind(ctx, |k| matches!(k, NodeKind::Load(_)));
    while let Some(node_id) = work.dequeue() {
        let [memory, addr] = ctx.node_inputs_exact::<2>(node_id)?;
        let [load_out] = ctx.node_outputs_exact::<1>(node_id)?;
        let Some(load_ty) = ctx.output_kind(load_out).as_value() else {
            continue;
        };
        let load_size = load_ty.byte_size() as i64;
        let Some(SpExpr::Terminal { base: _, offset }) =
            decompose_sp(ctx.graph_ref(), addr, sp_vn, &mut memo)
        else {
            continue;
        };
        let Some(j) = stack_arg_offsets.iter().position(|&k| k == offset) else {
            continue;
        };
        if disqualified.contains(&j) {
            continue;
        }
        let mut seen: entity_utils::DenseEntitySet<NodeOutputId> =
            entity_utils::DenseEntitySet::new();
        if mem_chain_is_dirty(
            ctx.as_view(),
            memory,
            offset,
            load_size,
            sp_vn,
            &mut memo,
            &mut seen,
            &mut shadow_memo,
        )? {
            disqualified.insert(j);
            groups.remove(&j);
            continue;
        }
        groups.entry(j).or_default().push(node_id);
    }

    // Strict contiguity from j=0 — first gap truncates.
    let mut max_j_plus_one = 0usize;
    while groups.contains_key(&max_j_plus_one) {
        max_j_plus_one += 1;
    }
    if max_j_plus_one == 0 {
        return Ok(OptimizationResult::NoChange);
    }

    let mut result = OptimizationResult::NoChange;
    for (j, &offset) in stack_arg_offsets.iter().enumerate().take(max_j_plus_one) {
        let index = (first_stack_arg + j) as u32;
        let Some(loads) = groups.remove(&j) else {
            continue;
        };

        // Space from first load (all loads in a K-group share the same memory
        // space).  Per-load output types may differ — pick the widest.
        let first = loads[0];
        let NodeKind::Load(space) = *ctx.node_kind(first) else {
            continue;
        };
        // Guard: every load in this K-group must share `space`. The grouping
        // logic above keys only on `j` (the offset slot), not on space, so a
        // multi-space lifter could in principle place two loads at the same
        // offset in different spaces. Skip the whole group on mismatch rather
        // than silently merging.
        if loads.iter().any(|&l| {
            !matches!(*ctx.node_kind(l), NodeKind::Load(s) if s == space)
        }) {
            continue;
        }
        // Collect (load, out_type) pairs and find the max byte size.
        let mut load_types: Vec<(NodeId, NodeOutputType)> = Vec::with_capacity(loads.len());
        for load in &loads {
            let [out] = ctx.node_outputs_exact::<1>(*load)?;
            let Some(ty) = ctx.output_kind(out).as_value() else {
                continue;
            };
            load_types.push((*load, ty));
        }
        let Some(max_type) = load_types.iter().map(|(_, t)| *t).max_by_key(|t| t.byte_size())
        else {
            continue;
        };

        let new_node = ctx.create_node(
            NodeKind::FunctionArg {
                source: FunctionArgSource::Stack { space, offset },
                index,
            },
            [],
            [NodeOutputKind::OutputType(max_type)],
        );
        let [new_out] = ctx.node_outputs_exact::<1>(new_node)?;

        for (load, load_ty) in load_types {
            let [old_out] = ctx.node_outputs_exact::<1>(load)?;
            if load_ty == max_type {
                // FunctionArg is exempt from the fingerprint check; no need
                // to absorb the load's fingerprint into it (and doing so
                // would couple FunctionArg's identity to the loads it
                // happens to subsume).
                result |= OptimizationResult::from_changed(ctx.replace_all_uses(old_out, new_out)?);
            } else {
                // Narrower read: insert a Truncate from the wider FunctionArg.
                // The Truncate is non-exempt and freshly created; inherit
                // the rewritten Load's fingerprint so the contributing
                // machine instruction's address survives the rewrite.
                let trunc = ctx.create_node_attributed(
                    NodeKind::Truncate,
                    [new_out],
                    [NodeOutputKind::OutputType(load_ty)],
                    &[load],
                );
                let [trunc_out] = ctx.node_outputs_exact::<1>(trunc)?;
                result |= OptimizationResult::from_changed(ctx.replace_all_uses(old_out, trunc_out)?);
            }
            ctx.detach_node_inputs(load);
        }
    }
    Ok(result)
}

/// Per-pass-call memo for [`mem_chain_is_dirty`]. Keyed on `(memory_token,
/// offset, load_size)`. Threaded through `detect_stack_args` so that two
/// stack-arg-load candidates sharing the same memory predecessor reuse the
/// walk's verdict.
type ShadowMemo = rustc_hash::FxHashMap<(NodeOutputId, i64, i64), bool>;

/// DFS through memory predecessors looking for a store that may shadow the
/// byte range `[offset, offset + load_size)`.  Treats `MemPhi` as a fork
/// where **every** value predecessor must be clean; `Call` / `CallOther` as
/// pass-throughs unless one of the call's value-args is an SP-rooted
/// pointer that escapes a stack slot into the callee — in that case the
/// callee may store through the pointer, so the chain is marked dirty.
///
/// Returns `true` if any path through the chain *may* overwrite bytes in the
/// load's range.  A `StackStore` or `StackStorePhi` whose byte range overlaps
/// the load's is treated as a shadow; one whose range is strictly disjoint is
/// walked past.
///
/// Plain `Store` nodes (those `StackStoreDetect` did not rewrite to
/// `StackStore` because their address didn't decompose to `sp + K`) are
/// alias-discriminated via [`crate::opt::sp_expr::decompose_sp`]: a non-SP-rooted
/// address is provably non-aliasing with the stack-arg space and the walker
/// passes through; an SP-rooted `Terminal` address uses the same byte-range
/// disjointness check as `StackStore`; an SP-rooted `Phi` address conservatively
/// terminates (matches `stack_load_forward::probe`'s posture).  This is the
/// `mem_chain_is_dirty` arm of cause #2 — gcc/clang at -O2 routinely
/// interleave volatile global writes between function-entry stack-arg loads
/// and the first uses, and without this branch they would all hit `_ => true`.
///
/// `StackStorePhi` offsets are per-predecessor and stored in
/// `Graph::stack_phi_offsets`.  They are relative to `InitialVar(sp)` by
/// construction (the only place that populates them is `StackStoreDetect`).
//
/// Iterative form of `mem_chain_is_dirty` — the prior recursive form
/// stack-overflowed on pathological deep prologues.
/// Walks the memory chain backward via an explicit work stack, with
/// dedicated frames for `MemPhi` predecessors that join-OR their
/// per-pred results.  Stack-safe at any chain depth and any phi
/// fan-out, including pathological 10k+ store prologues.
///
/// **Cycle handling.**  `seen` (a graph-wide visited set) is updated
/// on every visit; revisiting a `mem` returns `false` (clean) for
/// that edge, mirroring the original.  The original "cache only at
/// the outermost frame" trick (line 414) is replaced by the simpler
/// invariant that the iterative walk has a single entry-point frame
/// — we cache the final result for the original `mem` argument.
/// Sub-frame results aren't cached because their cleanliness depends
/// on the cycle set populated above them, not just on `(mem, offset,
/// load_size)`.
// Eight arguments are the minimum needed to thread cycle-guards, the
// SP-decomposition memo and the shadow-walk memo through the
// memory-chain DFS; bundling them into a context struct would just
// add indirection without clarifying call sites.
#[allow(clippy::too_many_arguments)]
fn mem_chain_is_dirty(
    ctx: crate::pattern::RewriteCtxView<'_>,
    mem: NodeOutputId,
    offset: i64,
    load_size: i64,
    sp_vn: rsleigh::Vn,
    sp_memo: &mut SpExprMemo,
    seen: &mut entity_utils::DenseEntitySet<NodeOutputId>,
    memo: &mut ShadowMemo,
) -> Result<bool> {
    let entry_key = (mem, offset, load_size);
    if let Some(&cached) = memo.get(&entry_key) {
        return Ok(cached);
    }

    struct DirtyStep<'a> {
        offset: i64,
        load_size: i64,
        sp_vn: rsleigh::Vn,
        sp_memo: &'a mut SpExprMemo,
    }
    impl<'a> MemChainStep for DirtyStep<'a> {
        type Verdict = bool;

        fn classify(
            &mut self,
            graph: &strider_ir::Graph,
            _mem: NodeOutputId,
            node: NodeId,
        ) -> Result<StepResult<bool>> {
            match *graph.node_kind(node) {
                NodeKind::InitialMemory => Ok(StepResult::Verdict(false)),
                NodeKind::StackStore { offset: k, .. } => Ok(
                    match step_through_stack_store(graph, node, k, self.offset, self.load_size) {
                        AliasStep::MayAlias => StepResult::Verdict(true),
                        AliasStep::PassThrough { prev_mem } => StepResult::Continue(prev_mem),
                    },
                ),
                NodeKind::StackStorePhi { .. } => Ok(
                    match step_through_stack_store_phi(graph, node, self.offset, self.load_size) {
                        AliasStep::MayAlias => StepResult::Verdict(true),
                        AliasStep::PassThrough { prev_mem } => StepResult::Continue(prev_mem),
                    },
                ),
                NodeKind::Store(_) => Ok(match step_through_store(
                    graph,
                    node,
                    self.sp_vn,
                    self.sp_memo,
                    self.offset,
                    self.load_size,
                ) {
                    AliasStep::MayAlias => StepResult::Verdict(true),
                    AliasStep::PassThrough { prev_mem } => StepResult::Continue(prev_mem),
                }),
                NodeKind::MemPhi => {
                    // Inputs: [PHI, MEM, MEM, ...].
                    let inputs = graph.node_inputs(node);
                    if inputs.len() < 2 {
                        return Err(anyhow::anyhow!(
                            "mem_chain_is_dirty: malformed MemPhi with zero predecessor inputs",
                        ));
                    }
                    let phi_token = inputs[0];
                    let preds = inputs.iter().skip(1).collect();
                    Ok(StepResult::JoinPhi {
                        phi_node: node,
                        phi_token,
                        preds,
                    })
                }
                NodeKind::Call | NodeKind::CallOther { .. } => {
                    let inputs = graph.node_inputs(node);
                    if inputs.len() < 2 {
                        // A `Call` / `CallOther` with fewer than 2
                        // inputs (control + memory) violates the
                        // signature contract.  Surface as Err
                        // rather than returning the unsafe "clean"
                        // direction (which would silently forward
                        // a stale value across the malformed call).
                        return Err(anyhow::anyhow!(
                            "mem_chain_is_dirty: malformed {:?} node with fewer than 2 inputs",
                            graph.node_kind(node),
                        ));
                    }
                    // Scan the call's value-args for SP-rooted pointers
                    // (`Add(InitialVar(sp), Const(K))` / equivalents).
                    // If any value-arg decomposes to a Terminal sp-rooted
                    // address, the caller has handed the callee a
                    // pointer into its own stack frame — the callee may
                    // store through it, so any subsequent load of any
                    // stack slot reachable from that pointer is a
                    // potential shadow.
                    //
                    // We don't know the callee's effective store extent,
                    // so model the escape as a write of `i64::MAX` bytes
                    // starting at the escaped offset.  `ranges_disjoint`
                    // saturates and reports "not disjoint" in that case,
                    // which collapses to: any sp-rooted escape pins the
                    // chain as dirty for any subsequent stack-arg load.
                    //
                    // `SpExpr::Phi` predecessors are checked one-by-one;
                    // any predecessor offset that is not provably
                    // disjoint pins the chain.
                    //
                    // `Call` inputs are `[CTRL, MEM, TARGET, ...args]`
                    // (value-args start at index 3); `CallOther` skips
                    // the explicit target since the user-op identity is
                    // encoded in the kind (value-args start at index 2).
                    // The outer match arm gates this branch to those two
                    // kinds.
                    let args_start = if matches!(graph.node_kind(node), NodeKind::Call) {
                        3
                    } else {
                        2
                    };
                    let load_offset = self.offset;
                    let load_size = self.load_size;
                    for arg in inputs.iter().skip(args_start) {
                        // `decompose_sp` returns `None` for any non-SP-rooted
                        // value (constants, register-derived integers,
                        // function-args, etc.).  Those args don't alias the
                        // caller's stack-arg slots, so we skip past them.
                        let Some(expr) = decompose_sp(graph, arg, self.sp_vn, self.sp_memo) else {
                            continue;
                        };
                        let offsets: &[i64] = match &expr {
                            SpExpr::Terminal { offset, .. } => std::slice::from_ref(offset),
                            SpExpr::Phi { offsets, .. } => offsets.as_slice(),
                        };
                        for &k in offsets {
                            if !ranges_disjoint(k, i64::MAX, load_offset, load_size) {
                                return Ok(StepResult::Verdict(true));
                            }
                        }
                    }
                    Ok(StepResult::Continue(inputs[1]))
                }
                _ => {
                    // Unknown memory-producing node: be conservative.
                    Ok(StepResult::Verdict(true))
                }
            }
        }

        fn cycle_verdict(&mut self) -> bool {
            // Cycle / re-visit: treat as clean for this edge.
            false
        }

        fn combine_phi(
            &mut self,
            _phi_node: NodeId,
            _phi_token: NodeOutputId,
            preds: Vec<bool>,
        ) -> bool {
            preds.into_iter().any(|d| d)
        }
    }

    let mut step = DirtyStep {
        offset,
        load_size,
        sp_vn,
        sp_memo,
    };
    let result = walk_mem_chain(
        ctx.graph_ref(),
        mem,
        CyclePolicy::GuardEveryNode,
        seen,
        |node| matches!(ctx.node_kind(node), NodeKind::MemPhi),
        &mut step,
    )?;
    memo.insert(entry_key, result);
    Ok(result)
}

#[cfg(test)]
mod tests;
