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
//!   `InitialVar(sp) + K` with `K == cc.stack_arg_offsets[j]`.  Reject any
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
        // SSoT: derive the positional-arg layout on-demand from the function's
        // own CC.  `first_stack_arg` is the register-vs-stack boundary; the
        // ranged clear below preserves the register-arg carriers recorded at
        // builder entry.
        let layout = ctx.function().default_cc().positional_arg_layout();
        let stack_vn = ctx.function().default_cc().stack_vn;
        let stack_arg_offsets: Vec<i64> = layout
            .iter()
            .filter_map(|e| match e {
                strider_target::PositionalArg::Stack { offset, .. } => Some(*offset),
                strider_target::PositionalArg::Register { .. } => None,
            })
            .collect();
        let first_stack_arg = layout
            .iter()
            .find_map(|e| match e {
                strider_target::PositionalArg::Stack { index, .. } => Some(*index as usize),
                strider_target::PositionalArg::Register { .. } => None,
            })
            .unwrap_or(layout.len());
        // Register args are recorded at builder entry; this pass owns only the
        // stack-arg indices (>= first_stack_arg). Clear just those so re-running
        // across stable iterations stays idempotent without wiping the
        // build-time register-arg carriers.
        ctx.function_mut()
            .clear_arg_values_from(first_stack_arg as u32);
        detect_stack_args(
            ctx,
            stack_vn,
            &stack_arg_offsets,
            first_stack_arg,
            alias_mode,
            calls_clobber_stack_arguments,
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
/// Returns the `InitialVar(sp)` output (the entry stack pointer), or `None`
/// when the function never reads it.  Stack-arg detection requires every
/// candidate load's terminal base to equal this value.
fn entry_sp_value(ctx: &mut crate::EditFunction<'_>, stack_vn: rsleigh::Vn) -> Option<ValueId> {
    // Exactly one `InitialVar(stack_vn)` exists (builder invariant), so the
    // search is order-independent.  Iterate the entry-reachable RPO
    // (`reverse_postorder_filter`) rather than the cached live set: after destructive passes
    // the live set is a superset of the entry-reachable set (a detached zombie
    // keeping its `InitialVar` pinned), so entry-reachable iteration skips
    // such zombies and preserves the original behaviour.
    for n in ctx.reverse_postorder_filter(|k| matches!(k, NodeKind::InitialVar(_))) {
        if matches!(*ctx.node_kind(n), NodeKind::InitialVar(vn) if vn == stack_vn) {
            let [out] = ctx
                .node_outputs_exact::<1>(n)
                .expect("InitialVar has 1 output per node signature");
            return Some(out);
        }
    }
    None
}

fn detect_stack_args(
    ctx: &mut crate::EditFunction<'_>,
    stack_vn: rsleigh::Vn,
    stack_arg_offsets: &[i64],
    first_stack_arg: usize,
    alias_mode: crate::AliasMode,
    calls_clobber_stack_arguments: bool,
    memo: &mut SpExprMemo,
) -> Result<()> {
    if stack_arg_offsets.is_empty() {
        return Ok(());
    }

    // Incoming stack args live at fixed offsets from the *entry* stack
    // pointer.  Pin `InitialVar(sp)` up front: a candidate load's terminal
    // base must equal it, so a load rooted at a *different* SP terminal —
    // e.g. an alignment-masked `sp & mask`, which addresses a frame local —
    // is rejected even when its offset coincides with a convention slot.
    // With no entry-SP read there can be no stack args.
    let Some(initial_sp) = entry_sp_value(ctx, stack_vn) else {
        return Ok(());
    };

    // Group candidate loads by their position `j` in `stack_arg_offsets`.
    // A load qualifies only if (a) its address decomposes to `sp + K` where
    // `K` is a convention offset, `sp` is the entry SP, and (b) nothing on
    // its memory chain may alias that slot (DFS shadow check).  If *any*
    // load at offset K is shadowed, the whole K-group is disqualified
    // (conservative).
    let mut shadow_memo: ShadowMemo = ShadowMemo::default();
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
        // A `Load` always produces a value output (validated signature).
        let load_ty = ctx
            .value_kind(load_value)
            .as_value()
            .expect("Load output is a value");
        let load_size = load_ty.byte_size() as i64;
        let Some(SpExpr { base, offset }) = decompose_sp(ctx.function(), addr, stack_vn, memo)
        else {
            continue;
        };
        // Only loads rooted at the entry SP are incoming stack args.
        if base != initial_sp {
            continue;
        }
        let Some(j) = stack_arg_offsets.iter().position(|&k| k == offset) else {
            continue;
        };
        if disqualified.contains(&j) {
            continue;
        }
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
        )?;
        if dirty {
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
        // mismatch rather than silently merging.  Every member came from the
        // `Load`-seeded worklist, so the kind is a structural invariant here.
        let first = loads[0];
        let NodeKind::Load(space) = *ctx.node_kind(first) else {
            unreachable!("group members are seeded from Load nodes");
        };
        if loads
            .iter()
            .any(|&l| !matches!(*ctx.node_kind(l), NodeKind::Load(s) if s == space))
        {
            continue;
        }

        // Register every qualifying Load's value as a carrier for arg `index`.
        // Each Load stays in place; consumers are not rewired.
        // Multiple Loads at the same offset (different widths) are all
        // recorded — the Vec<ValueId> per index accommodates this.
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
