//! Detects function arguments and records them in the
//! [`strider_ir::Function::arg_index_to_nodes`] side-table.
//!
//! Runs as a post-pass after the main fixed-point loop converges.  Identifies
//! register-passed arg reads (`InitialVar(arg_reg)`) and stack-passed arg
//! reads (`Load[InitialVar(sp) + K]` unshadowed by any prior store) and
//! records each underlying node in the side-table keyed by the argument's
//! index in the calling convention.  The original `InitialVar` / `Load` nodes
//! survive unchanged — no consumer rewiring, no new nodes.
//!
//! # Detection rules
//!
//! * **Register args** (no contiguity constraint).  For each register
//!   `R = cc.arg_passing_regs[i]`, if `InitialVar(R)` has live uses in the
//!   graph, register it as the carrier for arg `i` via
//!   `function.register_arg_node(i, initial_var_node)`.
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
//! For the stack-arg multi-`Load` case, every `Load` at the same `sp+K`
//! offset (potentially at different widths) is registered into the side-table
//! for that arg index — the `Vec<NodeId>` per entry accommodates this.

use strider_ir::node::{NodeId, NodeKind, NodeOutputId};

use crate::opt::error::Result;
use crate::opt::mem_walk::{CyclePolicy, MemChainStep, StepResult, walk_mem_chain};
use crate::opt::pipeline::{OptimizationResult, Optimizer};
use crate::opt::sp_expr::{
    AliasStep, SpExpr, SpExprMemo, decompose_sp, ranges_disjoint, step_through_store,
};
use crate::opt::stack_load_forward::is_stack_partition_input;
use crate::opt::worklist::seeded_kind;

/// Detects register-passed and stack-passed argument reads and records their
/// underlying carrier nodes in
/// [`strider_ir::Function::arg_index_to_nodes`] via
/// [`strider_ir::Function::register_arg_node`].  Intended to run once, as an
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
        function: &mut strider_ir::Function,
        entry: NodeId,
    ) -> Result<OptimizationResult> {
        let mut ctx = crate::pattern::RewriteCtx::new(function, entry);
        // `layout.register_args()` yields slots in ABI order, with
        // canonical positional indices stamped at layout-construction
        // time.  `layout.first_stack_index()` replaces the local
        // `arg_passing_regs.len()` derivation that used to live here.
        let arg_passing_regs: Vec<rsleigh::Vn> =
            self.layout.register_args().map(|(_, vn)| vn).collect();
        let stack_arg_offsets: Vec<i64> =
            self.layout.stack_args().map(|(_, o)| o).collect();
        detect_register_args(&mut ctx, &arg_passing_regs)?;
        detect_stack_args(
            &mut ctx,
            self.cc.stack_ptr_vn,
            &stack_arg_offsets,
            self.layout.first_stack_index() as usize,
        )?;
        // The pass only populates the arg_index_to_nodes side-table — it
        // does not rewrite the graph — so the optimizer's fixed-point loop
        // does not need to re-run.
        Ok(OptimizationResult::NoChange)
    }
}

/// Rule D: for every register in `arg_passing_regs` whose `InitialVar` node
/// has live uses, register that `InitialVar` as the carrier for arg `i` in
/// `function.arg_index_to_nodes`.  No contiguity check — reading only arg 2
/// still labels it arg 2.
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
/// reading of `reg`'s state).  The registered node carries the actual
/// sub-register Vn, so downstream consumers see the width the function
/// actually reads.
fn detect_register_args(
    ctx: &mut crate::pattern::RewriteCtx<'_>,
    arg_passing_regs: &[rsleigh::Vn],
) -> Result<()> {
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

    // Per-space bucket sorted by `(addr_off ascending, size descending)`.
    // Lets `largest_sub_in` binary-search to the first vn with
    // `addr_off >= lo` and then scan forward while `addr_off < hi`
    // — the sort order means the first vn with `addr_off == lo` is
    // the widest one (size-descending), and the scan terminates as
    // soon as we walk past `hi`.  Hot-loop complexity becomes
    // O(log V + matches) per arg slot instead of O(V).
    let initial_vars_by_space: rustc_hash::FxHashMap<rsleigh::VnSpace, Vec<(rsleigh::Vn, NodeId)>> = {
        let mut by_space: rustc_hash::FxHashMap<rsleigh::VnSpace, Vec<(rsleigh::Vn, NodeId)>> =
            rustc_hash::FxHashMap::default();
        for (&vn, &n) in &initial_vars {
            by_space.entry(vn.addr_space).or_default().push((vn, n));
        }
        for bucket in by_space.values_mut() {
            bucket.sort_by_key(|(vn, _)| (vn.addr_off, std::cmp::Reverse(vn.size)));
        }
        by_space
    };

    /// Find the largest `(Vn, NodeId)` whose Vn is fully contained
    /// in `reg`'s byte range.  Returns `None` if nothing's contained.
    ///
    /// Binary-searches the pre-sorted per-space bucket to the first
    /// candidate at `addr_off >= reg.addr_off`, then scans forward
    /// while the candidate's `addr_off` stays below `reg`'s end.
    fn largest_sub_in(
        initial_vars_by_space: &rustc_hash::FxHashMap<rsleigh::VnSpace, Vec<(rsleigh::Vn, NodeId)>>,
        reg: rsleigh::Vn,
    ) -> Option<(rsleigh::Vn, NodeId)> {
        let bucket = initial_vars_by_space.get(&reg.addr_space)?;
        let lo = reg.addr_off;
        let hi = reg.addr_off.checked_add(u64::from(reg.size))?;
        // First index whose `addr_off >= lo`.
        let start_idx = bucket.partition_point(|(vn, _)| vn.addr_off < lo);
        let mut best: Option<(rsleigh::Vn, NodeId)> = None;
        for (vn, n) in &bucket[start_idx..] {
            if vn.addr_off >= hi {
                break;
            }
            // Containment: `vn.addr_off >= lo` (guaranteed by start_idx)
            // and `vn.addr_off + vn.size <= hi`.
            if vn.addr_off.checked_add(u64::from(vn.size)).is_some_and(|e| e <= hi) {
                match best {
                    Some((b, _)) if b.size >= vn.size => {}
                    _ => best = Some((*vn, *n)),
                }
            }
        }
        best
    }

    for (i, reg) in arg_passing_regs.iter().enumerate() {
        // Exact match → use as-is.  Otherwise the largest sub-register
        // contained in `reg`'s byte range.
        let initial_var = if let Some(&n) = initial_vars.get(reg) {
            n
        } else if let Some((_, sub_n)) = largest_sub_in(&initial_vars_by_space, *reg) {
            sub_n
        } else {
            continue;
        };

        let [old_out] = ctx.node_outputs_exact::<1>(initial_var)?;
        // Skip if the InitialVar has no consumers.
        if ctx.output_use_cursor(old_out).current().is_none() {
            continue;
        }

        // Register the underlying InitialVar as the carrier for arg i.
        // The node stays in place; consumers are not rewired.
        ctx.function_mut().register_arg_node(i as u32, initial_var);
    }
    Ok(())
}

/// Rule (stack args): collect every `Load` node whose address decomposes to
/// `InitialVar(sp) + K` where `K` is one of the convention's stack-arg
/// offsets.  Group by `K`, then apply **strict contiguity** from position 0:
/// the first gap in the offset-set truncates, so surviving indices are a
/// gap-free prefix.  For each surviving group of qualifying `Load`s, register
/// every `Load` in the group into `function.arg_index_to_nodes` for that index.
///
/// The original `Load` nodes survive unchanged — no consumer rewiring.
/// Multiple `Load`s at the same `sp+K` offset (e.g. different widths) are all
/// registered into the side-table for that index.
fn detect_stack_args(
    ctx: &mut crate::pattern::RewriteCtx<'_>,
    sp_vn: rsleigh::Vn,
    stack_arg_offsets: &[i64],
    first_stack_arg: usize,
) -> Result<()> {
    if stack_arg_offsets.is_empty() {
        return Ok(());
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
            decompose_sp(ctx.function_ref(), addr, sp_vn, &mut memo)
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
        return Ok(());
    }

    for (j, _offset) in stack_arg_offsets.iter().enumerate().take(max_j_plus_one) {
        let index = (first_stack_arg + j) as u32;
        let Some(loads) = groups.remove(&j) else {
            continue;
        };

        // Guard: every load in this K-group must share the same memory space.
        // The grouping logic above keys only on `j` (the offset slot), not on
        // space, so a multi-space lifter could in principle place two loads at
        // the same offset in different spaces.  Skip the whole group on
        // mismatch rather than silently merging.
        let first = loads[0];
        let NodeKind::Load(space) = *ctx.node_kind(first) else {
            continue;
        };
        if loads.iter().any(|&l| {
            !matches!(*ctx.node_kind(l), NodeKind::Load(s) if s == space)
        }) {
            continue;
        }

        // Register every qualifying Load as a carrier for arg `index`.
        // Each Load stays in place; consumers are not rewired.
        // Multiple Loads at the same offset (different widths) are all
        // recorded — the Vec<NodeId> per index accommodates this.
        for load in loads {
            ctx.function_mut().register_arg_node(index, load);
        }
    }
    Ok(())
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
/// load's range.  A `Store` whose byte range overlaps the load's is treated as
/// a shadow; one whose range is strictly disjoint is walked past.
///
/// `Store` nodes are alias-discriminated via
/// [`crate::opt::sp_expr::decompose_sp`]: a non-SP-rooted address is provably
/// non-aliasing with the stack-arg space and the walker passes through; an
/// SP-rooted `Terminal` address uses byte-range disjointness; an SP-rooted
/// `Phi` address conservatively terminates.  This is the `mem_chain_is_dirty`
/// arm of cause #2 — gcc/clang at -O2 routinely interleave volatile global
/// writes between function-entry stack-arg loads and the first uses, and
/// without this branch they would all hit `_ => true`.
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
            graph: &strider_ir::Function,
            _mem: NodeOutputId,
            node: NodeId,
        ) -> Result<StepResult<bool>> {
            match *graph.node_kind(node) {
                NodeKind::InitialMemory => Ok(StepResult::Verdict(false)),
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
                NodeKind::MemPartition { .. } => {
                    // MemPartition: synthetic boundary tagging a unified
                    // memory edge with a single partition.  Pass through to
                    // the single unified-memory predecessor (input 0).
                    let inputs = graph.node_inputs(node);
                    if inputs.is_empty() {
                        // Malformed node — conservatively dirty.
                        return Ok(StepResult::Verdict(true));
                    }
                    Ok(StepResult::Continue(inputs[0]))
                }
                NodeKind::MemUnion => {
                    // MemUnion merges N partition-typed edges back into a
                    // single unified memory edge.  Only the Stack-partition
                    // input is relevant for the shadow walk; follow it and
                    // ignore the rest.  If no Stack-partition input exists
                    // the chain is opaque — conservatively dirty.
                    let inputs = graph.node_inputs(node);
                    match inputs
                        .iter()
                        .find(|&inp| is_stack_partition_input(graph, inp))
                    {
                        Some(stack_input) => Ok(StepResult::Continue(stack_input)),
                        None => Ok(StepResult::Verdict(true)),
                    }
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
        ctx.function_ref(),
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
