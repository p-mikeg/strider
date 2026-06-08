//! Detects stack-passed function arguments and records them in the
//! [`strider_ir::Function::arg_index_to_values`] side-table.
//!
//! Runs as a post-pass after the main fixed-point loop converges.  Register-
//! passed arg carriers are recorded unconditionally at builder entry (by
//! `FunctionBuilder::set_entry_region`); this pass handles only the stack-arg
//! portion, which genuinely requires the optimized memory graph.
//!
//! Stack-passed arg `Load` nodes (`Load[InitialVar(sp) + K]` unshadowed by
//! any prior store) are detected and recorded in the side-table.  The
//! original `Load` nodes survive as the registered carriers — no consumer
//! rewiring, no new nodes.  (The shared memory-SSA walk used for the
//! stack-arg shadow check may narrow a stack-arg `Load`'s own memory input
//! onto its nearest clobber; this never changes which args are detected.)
//!
//! # Detection rules
//!
//! * **Stack args** (strict contiguity + no-shadow).  Collect all `Load`
//!   nodes whose address decomposes (via [`sp_expr::decompose_sp`]) to
//!   `InitialVar(sp) + K` where `K` maps to a slot under the convention's
//!   [`strider_target::StackArgs`] formula (`StackArgs::index_of`).  Reject any
//!   whose memory input is reachable backward from a shadowing store — the
//!   walk is a DFS through memory predecessors that treats `MemPhi` as a
//!   fork where every predecessor must be non-disqualifying.  Disqualifying
//!   nodes: a stack-tagged `Store { offset: K }`, a `MemPhi` whose
//!   per-predecessor offsets contain `K`, and un-decomposed `Store` (may alias —
//!   conservative).  Non-disqualifying: `InitialMemory`, `Call`,
//!   `CallOther`, and stores at other offsets.  After
//!   filtering, emit only those indices that form a gap-free prefix starting
//!   at `first_stack_arg = arg_passing_regs.len()`; the first gap truncates.
//!
//! For the stack-arg multi-`Load` case, every `Load` at the same `sp+K`
//! offset (potentially at different widths) is registered into the side-table
//! for that arg index — the `Vec<ValueId>` per entry accommodates this.

use strider_ir::IRViewer;
use strider_ir::node::{NodeId, NodeKind, ValueId};

use crate::error::Result;
use crate::pipeline::{OptimizationResult, Optimizer};
use crate::sp_expr::{AddrClass, SpAliasOracle, SpExpr, SpExprMemo, decompose_sp};
use crate::worklist::seeded_kind;

/// Detects stack-passed argument `Load` nodes and records their
/// carrier nodes in
/// [`strider_ir::Function::arg_index_to_values`] via
/// [`strider_ir::Function::register_arg_value`].  Intended to run once, as an
/// [`OptimizerPipeline::add_post_pass`][crate::OptimizerPipeline::add_post_pass]
/// after the fixed-point loop has converged.
///
/// Register-arg carriers are recorded at builder entry
/// (`FunctionBuilder::set_entry_region`); this pass handles only the
/// stack-arg portion (indices `>= first_stack_arg`), which genuinely
/// requires the optimized memory graph.  The arg layout (stack-arg offsets
/// and the register-vs-stack boundary) is derived on-demand from the
/// function's own calling convention (`Function::default_cc`), the
/// stack-pointer varnode likewise, and the alias precision / call-clobber
/// behaviour from [`crate::OptCtx`] — the pass carries no configuration
/// of its own.
#[derive(Clone, Default)]
pub struct FunctionArgDetect;

impl FunctionArgDetect {
    /// Creates the pass.  The arg layout, stack pointer, alias precision,
    /// and call-clobber behaviour all come from the function / shared
    /// [`crate::OptCtx`] at apply time.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Optimizer for FunctionArgDetect {
    fn apply(
        &self,
        ctx: &mut crate::EditFunction<'_>,
        opt_ctx: &mut crate::OptCtx<'_>,
    ) -> Result<OptimizationResult> {
        let alias_mode = opt_ctx.options.alias_mode;
        let calls_clobber_stack_arguments = opt_ctx.options.calls_clobber_stack_arguments;
        let args_assume_distinct_sp_bases_disjoint =
            opt_ctx.options.args_assume_distinct_sp_bases_disjoint;
        // SSoT: derive the positional-arg layout on-demand from the function's
        // own CC.  `first_stack_arg` is the register-vs-stack boundary; the
        // ranged clear below preserves the register-arg carriers recorded at
        // builder entry.
        let layout = ctx.function().default_cc().positional_arg_layout();
        let stack_vn = ctx.function().default_cc().stack_vn;
        let first_stack_arg = layout.first_stack_index();
        let Some(stack_args) = layout.stack else {
            // This convention passes no arguments on the stack.
            return Ok(OptimizationResult::NoChange);
        };
        // Register args are recorded at builder entry; this pass owns only the
        // stack-arg indices (>= first_stack_arg). Clear just those so re-running
        // across stable iterations stays idempotent without wiping the
        // build-time register-arg carriers.
        ctx.function_mut()
            .clear_arg_values_from(first_stack_arg as u32);
        detect_stack_args(
            ctx,
            stack_args,
            stack_vn,
            first_stack_arg,
            alias_mode,
            calls_clobber_stack_arguments,
            args_assume_distinct_sp_bases_disjoint,
            &mut opt_ctx.sp_memo,
        )?;
        // Arg detection only populates the arg_index_to_values side-table,
        // and the memory-SSA walk's narrowing only shortens stack-arg loads'
        // memory edges (idempotent, never changes which args are detected) —
        // so as a post-pass it reports `NoChange`: nothing here unlocks
        // further optimization that would require another fixed-point pass.
        Ok(OptimizationResult::NoChange)
    }
}

/// Rule (stack args): collect every `Load` node whose address decomposes to
/// `InitialVar(sp) + K` where `K` is one of the convention's stack-arg
/// offsets.  Group by `K`, then apply **strict contiguity** from position 0:
/// the first gap in the offset-set truncates, so surviving indices are a
/// gap-free prefix.  For each surviving group of qualifying `Load`s, register
/// every `Load` in the group into `function.arg_index_to_values` for that index.
///
/// The original `Load` nodes survive unchanged — no consumer rewiring.
/// Multiple `Load`s at the same `sp+K` offset (e.g. different widths) are all
/// registered into the side-table for that index.
#[allow(clippy::too_many_arguments)]
fn detect_stack_args(
    ctx: &mut crate::EditFunction<'_>,
    stack_args: strider_target::StackArgs,
    stack_vn: rsleigh::Vn,
    first_stack_arg: usize,
    alias_mode: crate::AliasMode,
    calls_clobber_stack_arguments: bool,
    args_assume_distinct_sp_bases_disjoint: bool,
    memo: &mut SpExprMemo,
) -> Result<()> {
    // Incoming stack args live at fixed offsets from the *entry* stack
    // pointer.  Pin `InitialVar(sp)` up front: a candidate load's terminal
    // base must equal it, so a load rooted at a *different* SP terminal —
    // e.g. an alignment-masked `sp & mask`, which addresses a frame local —
    // is rejected even when its offset coincides with a convention slot.
    // With no entry-SP read there can be no stack args.
    let Some(initial_sp) = ctx.function().initial_sp_value() else {
        return Ok(());
    };
    let mut shadow_memo: ShadowMemo = ShadowMemo::default();
    // Group qualifying loads by stack-arg slot index. A load qualifies when:
    //   (a) its address decomposes to `initial_sp + K`,
    //   (b) `K` maps to a slot (StackArgs::index_of — range inside one slot), and
    //   (c) nothing on its memory chain clobbers the slot (mem_chain_is_dirty
    //       resolves the nearest clobber via the SpAliasOracle + the knobs;
    //       not-dirty == the nearest clobber is InitialMemory).
    let mut groups: rustc_hash::FxHashMap<usize, Vec<NodeId>> = rustc_hash::FxHashMap::default();
    let mut disqualified: rustc_hash::FxHashSet<usize> = rustc_hash::FxHashSet::default();
    let mut work = seeded_kind(ctx, |k| matches!(k, NodeKind::Load(_)));
    while let Some(node_id) = work.dequeue() {
        let [memory, addr] = ctx
            .graph_ref()
            .node_inputs_exact::<2>(node_id)
            .expect("Load has 2 inputs per node signature");
        let [load_value] = ctx
            .node_outputs_exact::<1>(node_id)
            .expect("Load has 1 output per node signature");
        let Some(load_ty) = ctx.value_kind(load_value).as_value() else { continue };
        let load_size = load_ty.byte_size() as i64;
        // (a) decompose to initial_sp + K.
        let Some(SpExpr { base, offset }) = decompose_sp(ctx.function(), addr, stack_vn, memo)
        else {
            continue;
        };
        if base != initial_sp {
            continue;
        }
        // (b) K maps to a slot, range inside one slot.
        let Some(slot) = stack_args.index_of(offset, load_size) else {
            continue;
        };
        if disqualified.contains(&slot) {
            continue;
        }
        // (c) memory chain clean.
        let dirty = mem_chain_is_dirty(
            ctx,
            node_id,
            memory,
            base,
            offset,
            load_size,
            memo,
            &mut shadow_memo,
            alias_mode,
            calls_clobber_stack_arguments,
            args_assume_distinct_sp_bases_disjoint,
        )?;
        if dirty {
            disqualified.insert(slot);
            groups.remove(&slot);
            continue;
        }
        groups.entry(slot).or_default().push(node_id);
    }

    // Strict contiguity from slot 0 — first gap (or disqualified slot) truncates.
    let mut max_slot_plus_one = 0usize;
    while groups.contains_key(&max_slot_plus_one) {
        max_slot_plus_one += 1;
    }
    for slot in 0..max_slot_plus_one {
        let index = (first_stack_arg + slot) as u32;
        let Some(loads) = groups.remove(&slot) else { continue };
        // Same-space guard (preserved from the previous implementation).
        let first = loads[0];
        let NodeKind::Load(space) = *ctx.node_kind(first) else {
            unreachable!("group members are seeded from Load nodes");
        };
        if loads.iter().any(|&l| !matches!(*ctx.node_kind(l), NodeKind::Load(s) if s == space)) {
            continue;
        }
        for load in loads {
            let [load_value] = ctx
                .node_outputs_exact::<1>(load)
                .expect("Load has 1 output per node signature");
            ctx.register_arg_value(index, load_value);
        }
    }
    Ok(())
}

/// Per-pass-call memo for [`mem_chain_is_dirty`]. Keyed on `(memory_token,
/// base, offset, load_size)`. Threaded through `detect_stack_args` so that
/// two stack-arg-load candidates sharing the same memory predecessor, SP
/// base, and slot reuse the walk's verdict.
type ShadowMemo = rustc_hash::FxHashMap<(ValueId, ValueId, i64, i64), bool>;

/// Walks the memory chain backward from `mem` looking for any def that
/// may shadow the byte range `[offset, offset + load_size)`.  Returns
/// `true` if any path through the chain may overwrite bytes in the
/// load's range.
///
/// Delegates the traversal (cycle-guarded, MemPhi-forking, stack-safe at
/// any chain depth) to [`may_clobber`]; the per-def shadow verdict comes
/// from the shared [`SpAliasOracle`] with the candidate load's
/// `AddrClass::SpRooted { base, offset }` class.  Memoised per pass-call
/// on `(mem, base, offset, load_size)`.
#[allow(clippy::too_many_arguments)]
fn mem_chain_is_dirty(
    ctx: &mut crate::EditFunction<'_>,
    load: NodeId,
    mem: ValueId,
    base: ValueId,
    offset: i64,
    load_size: i64,
    sp_memo: &mut SpExprMemo,
    memo: &mut ShadowMemo,
    alias_mode: crate::AliasMode,
    calls_clobber_stack_arguments: bool,
    args_assume_distinct_sp_bases_disjoint: bool,
) -> Result<bool> {
    let entry_key = (mem, base, offset, load_size);
    if let Some(&cached) = memo.get(&entry_key) {
        return Ok(cached);
    }

    let mut oracle = SpAliasOracle {
        load_class: AddrClass::SpRooted { base, offset },
        load_size,
        sp_memo,
        alias_mode,
        call_clobbers: calls_clobber_stack_arguments,
        distinct_sp_bases_disjoint: args_assume_distinct_sp_bases_disjoint,
    };
    // Walk from the def that produced the load's memory input.  The oracle
    // does not consult the load node (the slot range is carried by
    // `offset`/`load_size`), but `may_clobber` uses it to narrow the load's
    // memory edge onto the nearest clobber.  The chain is dirty iff that
    // nearest clobber is anything but the clean `InitialMemory` root.
    let start = ctx.function().producer(mem);
    let clobber = crate::memory_ssa::may_clobber(ctx, &mut oracle, load, start);
    let result = !matches!(ctx.node_kind(clobber), NodeKind::InitialMemory);
    memo.insert(entry_key, result);
    Ok(result)
}

#[cfg(test)]
mod tests;
