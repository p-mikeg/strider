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
//!   `R = cc.arg_passing_regs()[i]`, if `InitialVar(R)` has live uses in the
//!   graph, emit one `FunctionArg { Register(R), i }` and rewire every use
//!   of `InitialVar(R)`'s output to point at the new node.
//!
//! * **Stack args** (strict contiguity + no-shadow).  Collect all `Load`
//!   nodes whose address decomposes (via [`sp_expr::decompose_sp`]) to
//!   `InitialVar(sp) + K` with `K == cc.stack_arg_offsets()[j]`.  Reject any
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

use ir::node::{FunctionArgSource, NodeId, NodeKind, NodeOutputId, NodeOutputKind, NodeOutputType};

use crate::error::Result;
use crate::pipeline::{OptimizationResult, OptimizerOnBuilt};
use crate::sp_expr::{
    AliasStep, SpExpr, SpExprMemo, decompose_sp, step_through_stack_store,
    step_through_stack_store_phi, step_through_store,
};
use crate::worklist::WorkSet;

/// Replaces register-passed and stack-passed argument reads with canonical
/// [`NodeKind::FunctionArg`][ir::node::NodeKind::FunctionArg] nodes.  Intended
/// to run once, as an
/// [`OptimizerPipeline::add_post_pass`][crate::OptimizerPipeline::add_post_pass]
/// after the fixed-point loop has converged.
#[derive(Clone)]
pub struct FunctionArgDetect {
    /// Varnodes (in positional order) used by the calling convention to pass
    /// integer arguments in registers.  Entry `i` is arg `i`.
    pub arg_passing_regs: Vec<rsleigh::Vn>,
    /// Varnode of the stack pointer register (used to recognise SP-relative
    /// stack-arg loads).
    pub stack_ptr_vn: rsleigh::Vn,
    /// Positional byte offsets of stack-passed arguments from the entry-time
    /// stack pointer.  Entry `j` is the offset of stack-arg `j`, which has
    /// overall argument index `arg_passing_regs.len() + j`.
    pub stack_arg_offsets: Vec<i64>,
}

impl FunctionArgDetect {
    /// Creates a new pass with an explicit register list, stack-pointer
    /// varnode, and stack-arg offset table.
    #[must_use]
    pub fn new(
        arg_passing_regs: Vec<rsleigh::Vn>,
        stack_ptr_vn: rsleigh::Vn,
        stack_arg_offsets: Vec<i64>,
    ) -> Self {
        Self {
            arg_passing_regs,
            stack_ptr_vn,
            stack_arg_offsets,
        }
    }

    /// Creates a new pass whose parameters are taken from the supplied
    /// calling convention.
    #[must_use]
    pub fn from_convention(cc: &target::BuiltCallingConvention) -> Self {
        Self::new(
            cc.arg_passing_regs().to_vec(),
            cc.stack_ptr_vn(),
            cc.stack_arg_offsets().to_vec(),
        )
    }
}

impl OptimizerOnBuilt for FunctionArgDetect {
    fn optimize_built(&self, function: &mut pattern::RewriteCtx<'_>) -> Result<OptimizationResult> {
        let mut changed = OptimizationResult::NoChange;
        changed |= detect_register_args(function, &self.arg_passing_regs)?;
        changed |= detect_stack_args(
            function,
            self.stack_ptr_vn,
            &self.stack_arg_offsets,
            self.arg_passing_regs.len(),
        )?;
        // Replacing `Load[sp+K]` with `FunctionArg` orphans the address-
        // computation chain (`Add(sp, K)` and friends).  Those nodes remain in
        // the use-list of surviving producers like `InitialVar(sp)`, which
        // confuses downstream consumers that walk use-lists — e.g. the dot
        // renderer draws an edgeless `InitialVar(sp)` island.  Detach them.
        // The detach result is hygiene-only (post-pass return values are
        // ignored by the pipeline); don't escalate it into `Changed`.
        let _ = crate::worklist::detach_unreachable_nodes(function.graph, function.entry);
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
    fg: &mut pattern::RewriteCtx<'_>,
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
    for n in fg.preorder() {
        if let NodeKind::InitialVar(vn) = *fg.node_kind(n) {
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

        let [old_out] = fg.node_outputs_exact::<1>(initial_var)?;
        // Skip if the InitialVar has no consumers.
        if fg.output_use_cursor(old_out).current().is_none() {
            continue;
        }

        let out_type = NodeOutputType::try_from(effective_vn.size)?;
        let new_node = fg.create_node(
            NodeKind::FunctionArg {
                source: FunctionArgSource::Register(effective_vn),
                index: i as u32,
            },
            [],
            [NodeOutputKind::OutputType(out_type)],
        );
        let [new_out] = fg.node_outputs_exact::<1>(new_node)?;
        // Inherit the InitialVar's asm-fingerprint so downstream pattern
        // queries (`m.asm_fingerprint(c, &graph)` on a captured FunctionArg)
        // can still trace back to the contributing machine instruction.
        // FunctionArg is exempt from the validator's non-empty-fingerprint
        // check, but the superset-only contract still says passes may grow
        // fingerprints — never shrink them — when replacing a node's uses.
        // The single-source register-args path (one InitialVar in, one
        // FunctionArg out) carries no coupling concern; the stack-args path
        // unifies multiple Loads and intentionally skips the absorption.
        fg.extend_asm_fingerprint_from(new_node, initial_var);
        result |= OptimizationResult::from_changed(fg.replace_all_uses(old_out, new_out)?);
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
/// Slice 4 extends this with memory-shadow disqualification; slice 5 extends
/// it with width merging.
fn detect_stack_args(
    fg: &mut pattern::RewriteCtx<'_>,
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
    let mut work = WorkSet::seeded_kind(fg, |k| matches!(k, NodeKind::Load(_)));
    while let Some(node_id) = work.pop() {
        let [memory, addr] = fg.node_inputs_exact::<2>(node_id)?;
        let [load_out] = fg.node_outputs_exact::<1>(node_id)?;
        let Some(load_ty) = fg.output_kind(load_out).as_value() else {
            continue;
        };
        let load_size = load_ty.byte_size() as i64;
        let mut visiting: entity_utils::DenseEntitySet<ir::node::NodeId> = entity_utils::DenseEntitySet::new();
        let Some(SpExpr::Terminal { base: _, offset }) =
            decompose_sp(fg.graph, addr, sp_vn, &mut memo, &mut visiting)
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
            fg.as_view(),
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
        let NodeKind::Load(space) = *fg.node_kind(first) else {
            continue;
        };
        // Guard: every load in this K-group must share `space`. The grouping
        // logic above keys only on `j` (the offset slot), not on space, so a
        // multi-space lifter could in principle place two loads at the same
        // offset in different spaces. Skip the whole group on mismatch rather
        // than silently merging.
        if loads.iter().any(|&l| {
            !matches!(*fg.node_kind(l), NodeKind::Load(s) if s == space)
        }) {
            continue;
        }
        // Collect (load, out_type) pairs and find the max byte size.
        let mut load_types: Vec<(NodeId, NodeOutputType)> = Vec::with_capacity(loads.len());
        for load in &loads {
            let [out] = fg.node_outputs_exact::<1>(*load)?;
            let Some(ty) = fg.output_kind(out).as_value() else {
                continue;
            };
            load_types.push((*load, ty));
        }
        let Some(max_type) = load_types.iter().map(|(_, t)| *t).max_by_key(|t| t.byte_size())
        else {
            continue;
        };

        let new_node = fg.create_node(
            NodeKind::FunctionArg {
                source: FunctionArgSource::Stack { space, offset },
                index,
            },
            [],
            [NodeOutputKind::OutputType(max_type)],
        );
        let [new_out] = fg.node_outputs_exact::<1>(new_node)?;

        for (load, load_ty) in load_types {
            let [old_out] = fg.node_outputs_exact::<1>(load)?;
            if load_ty == max_type {
                // FunctionArg is exempt from the fingerprint check; no need
                // to absorb the load's fingerprint into it (and doing so
                // would couple FunctionArg's identity to the loads it
                // happens to subsume).
                result |= OptimizationResult::from_changed(fg.replace_all_uses(old_out, new_out)?);
            } else {
                // Narrower read: insert a Truncate from the wider FunctionArg.
                let trunc = fg.create_node(
                    NodeKind::Truncate,
                    [new_out],
                    [NodeOutputKind::OutputType(load_ty)],
                );
                let [trunc_out] = fg.node_outputs_exact::<1>(trunc)?;
                // The Truncate is non-exempt and freshly created; inherit
                // the rewritten Load's fingerprint so the contributing
                // machine instruction's address survives the rewrite.
                fg.extend_asm_fingerprint_from(trunc, load);
                result |= OptimizationResult::from_changed(fg.replace_all_uses(old_out, trunc_out)?);
            }
            fg.detach_node_inputs(load);
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
/// pass-throughs (a caller cannot alias the callee's incoming stack-arg area
/// through a nested call).
///
/// Returns `true` if any path through the chain *may* overwrite bytes in the
/// load's range.  A `StackStore` or `StackStorePhi` whose byte range overlaps
/// the load's is treated as a shadow; one whose range is strictly disjoint is
/// walked past.
///
/// Plain `Store` nodes (those `StackStoreDetect` did not rewrite to
/// `StackStore` because their address didn't decompose to `sp + K`) are
/// alias-discriminated via [`crate::sp_expr::decompose_sp`]: a non-SP-rooted
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
/// Iterative form of `mem_chain_is_dirty` — was recursive (scale.md A3).
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
    fg: pattern::RewriteCtxView<'_>,
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

    /// Work-stack frame.  Either a fresh `Visit` of a mem node, or a
    /// `JoinPhi` continuation that OR-combines K already-popped
    /// predecessor results into the phi's own result.
    enum Frame {
        Visit(NodeOutputId),
        /// After visiting all `pred_count` predecessors of a MemPhi,
        /// pop their `bool` results from `results` and OR them.
        /// `pred_count` is the number of predecessor `Visit` frames
        /// we pushed; results stack invariant: top `pred_count`
        /// entries belong to this phi's preds.
        JoinPhi { pred_count: usize },
    }

    let mut work: Vec<Frame> = vec![Frame::Visit(mem)];
    let mut results: Vec<bool> = Vec::new();

    while let Some(frame) = work.pop() {
        match frame {
            Frame::JoinPhi { pred_count } => {
                let drain_at = results.len() - pred_count;
                let any_dirty = results.drain(drain_at..).any(|d| d);
                results.push(any_dirty);
            }
            Frame::Visit(cur_mem) => {
                if !seen.insert(cur_mem) {
                    // Cycle / re-visit: treat as clean for this edge.
                    results.push(false);
                    continue;
                }
                let node = fg.get_node_from_output(cur_mem);
                match *fg.node_kind(node) {
                    NodeKind::InitialMemory => {
                        results.push(false);
                    }
                    NodeKind::StackStore { offset: k, .. } => {
                        match step_through_stack_store(fg.graph, node, k, offset, load_size) {
                            AliasStep::MayAlias => results.push(true),
                            AliasStep::PassThrough { prev_mem } => {
                                work.push(Frame::Visit(prev_mem));
                            }
                        }
                    }
                    NodeKind::StackStorePhi { .. } => {
                        match step_through_stack_store_phi(fg.graph, node, offset, load_size) {
                            AliasStep::MayAlias => results.push(true),
                            AliasStep::PassThrough { prev_mem } => {
                                work.push(Frame::Visit(prev_mem));
                            }
                        }
                    }
                    NodeKind::Store(_) => {
                        match step_through_store(fg.graph, node, sp_vn, sp_memo, offset, load_size)
                        {
                            AliasStep::MayAlias => results.push(true),
                            AliasStep::PassThrough { prev_mem } => {
                                work.push(Frame::Visit(prev_mem));
                            }
                        }
                    }
                    NodeKind::MemPhi => {
                        // Inputs: [PHI, MEM, MEM, ...].  Push a JoinPhi
                        // continuation followed by every predecessor's
                        // Visit frame.  When the LIFO worklist pops them,
                        // each pred runs to completion (pushing one
                        // result), and the JoinPhi at the bottom OR-combines.
                        let inputs: Vec<NodeOutputId> =
                            fg.node_inputs(node).into_iter().collect();
                        let preds: Vec<NodeOutputId> = inputs.into_iter().skip(1).collect();
                        let pred_count = preds.len();
                        if pred_count == 0 {
                            // Empty phi violates the MemPhi signature
                            // contract (variadic mem-tail must be ≥ 1).
                            // Surface as Err rather than returning the
                            // unsafe "clean" direction.
                            return Err(anyhow::anyhow!(
                                "mem_chain_is_dirty: malformed MemPhi with zero predecessor inputs",
                            ));
                        }
                        work.push(Frame::JoinPhi { pred_count });
                        for pred in preds {
                            work.push(Frame::Visit(pred));
                        }
                    }
                    NodeKind::Call | NodeKind::CallOther { .. } => {
                        let inputs = fg.node_inputs(node);
                        if inputs.len() < 2 {
                            // A `Call` / `CallOther` with fewer than 2
                            // inputs (control + memory) violates the
                            // signature contract.  Surface as Err
                            // rather than returning the unsafe "clean"
                            // direction (which would silently forward
                            // a stale value across the malformed call).
                            return Err(anyhow::anyhow!(
                                "mem_chain_is_dirty: malformed {:?} node with fewer than 2 inputs",
                                fg.node_kind(node),
                            ));
                        }
                        work.push(Frame::Visit(inputs[1]));
                    }
                    _ => {
                        // Unknown memory-producing node: be conservative.
                        results.push(true);
                    }
                }
            }
        }
    }

    // The walk pushed exactly one final result for the original `mem`.
    // Round 10 H10-S6 (R10-2C): surface the invariant violation as `Err`
    // instead of silently assuming `true` in release builds.  Any
    // result-stack count other than 1 is a walker bug, not a property
    // of the input graph.
    if results.len() != 1 {
        return Err(anyhow::anyhow!(
            "mem_chain_is_dirty: result-stack invariant broken — expected 1 final result, \
             got {} (walker bug)",
            results.len()
        ));
    }
    let Some(result) = results.pop() else {
        // unreachable: the len==1 check above already proved
        // `results` is non-empty.
        return Err(anyhow::anyhow!(
            "mem_chain_is_dirty: result-stack pop failed after len==1 check (walker bug)"
        ));
    };
    memo.insert(entry_key, result);
    Ok(result)
}

#[cfg(test)]
mod tests;
