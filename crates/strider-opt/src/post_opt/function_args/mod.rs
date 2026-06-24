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
//!   `InitialVar(sp) + K` where `K` falls in a stack slot under the
//!   convention's [`strider_target::StackArgs`] formula (`StackArgs::slot_of`
//!   floors the load's first byte onto its containing slot — a wider-than-slot
//!   argument such as a 32-bit-ABI `double` is anchored at the slot its first
//!   byte occupies).  Reject any
//!   whose memory input is reachable backward from a shadowing store — the
//!   walk is a DFS through memory predecessors that treats `MemPhi` as a
//!   fork where every predecessor must be non-disqualifying.  Disqualifying
//!   nodes: a stack-tagged `Store { offset: K }`, a `MemPhi` whose
//!   per-predecessor offsets contain `K`, and un-decomposed `Store` (may alias —
//!   conservative).  Non-disqualifying: `InitialMemory`, `Call`,
//!   `CallOther`, and stores at other offsets.  After
//!   filtering, a width-aware cursor walks the surviving byte-position slots
//!   from 0, assigning one *argument ordinal* per anchored argument: a
//!   wider-than-slot argument consumes every slot it spans (so its footprint
//!   is not mistaken for a gap) yet advances the ordinal by exactly one.
//!   Ordinals start at `first_stack_arg = arg_passing_regs.len()`; the first
//!   slot with no anchored load ends the gap-free prefix.
//!
//! For the stack-arg multi-`Load` case, every `Load` touching one argument's
//! slot span (potentially at different widths / sub-field offsets) is
//! registered into the side-table for that argument ordinal — the
//! `Vec<ValueId>` per entry accommodates this.

use strider_ir::IRViewer;
use strider_ir::node::{NodeId, NodeKind, ValueId};

use crate::error::Result;
use crate::pipeline::PostOptimizer;
use crate::sp_expr::{AddrClass, SpAliasCfg, SpDecomposer, SpExpr, SpExprMemo};

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
#[derive(Clone)]
pub struct FunctionArgDetect;

impl PostOptimizer for FunctionArgDetect {
    fn apply(
        &self,
        ctx: &mut crate::EditFunction<'_>,
        opt_ctx: &mut crate::OptCtx<'_>,
    ) -> Result<()> {
        // SSoT: derive the positional-arg layout on-demand from the function's
        // own CC.  `first_stack_arg` is the register-vs-stack boundary; the
        // ranged clear below preserves the register-arg carriers recorded at
        // builder entry.
        let cc = ctx.function().default_cc();
        let first_stack_arg = cc.arg_passing_regs.len();
        let maybe_stack_args = cc.stack_args;
        let Some(stack_args) = maybe_stack_args else {
            // This convention passes no arguments on the stack.
            return Ok(());
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
            first_stack_arg,
            opt_ctx.options.alias_mode,
            opt_ctx.options.mem_alias,
            &mut opt_ctx.sp_memo,
        )?;
        // Arg detection only populates the arg_index_to_values side-table,
        // and the memory-SSA walk's narrowing only shortens stack-arg loads'
        // memory edges (idempotent, never changes which args are detected).
        Ok(())
    }
}

/// Rule (stack args): collect every `Load` node whose address decomposes to
/// `InitialVar(sp) + K` where `K` lands in a stack slot.  Group by the
/// byte-position slot the load's first byte occupies (`StackArgs::slot_of`,
/// a plain floor), tracking how far each anchored load reaches.  A width-aware
/// cursor then walks slots from 0, mapping each anchored argument to one
/// *ordinal*: a wider-than-slot argument (e.g. a 32-bit-ABI `double` spanning
/// two slots) advances the cursor across all its slots but the ordinal by one,
/// so the following narrower argument is not lost to the slots the wide one
/// covered.  The first slot with no anchored load ends the gap-free prefix.
/// For each argument, every qualifying `Load` touching its slot span is
/// registered into `function.arg_index_to_values` for that ordinal.
///
/// The original `Load` nodes survive unchanged — no consumer rewiring.
/// Multiple `Load`s touching one argument (e.g. different widths or sub-field
/// offsets) are all registered into the side-table for that ordinal.
fn detect_stack_args(
    ctx: &mut crate::EditFunction<'_>,
    stack_args: strider_target::StackArgs,
    first_stack_arg: usize,
    alias_mode: crate::AliasMode,
    mem_opts: crate::MemAliasOptions,
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
    // Group qualifying loads by the *byte-position* slot their first byte
    // lands in (`StackArgs::slot_of` — a plain floor, no upper size bound).  A
    // load qualifies when:
    //   (a) its address decomposes to `initial_sp + K`,
    //   (b) `K` is at or above the first stack slot (StackArgs::slot_of), and
    //   (c) nothing on its memory chain clobbers the slot (mem_chain_is_dirty
    //       resolves the nearest clobber via the SpAliasCfg + the knobs;
    //       not-dirty == the nearest clobber is InitialMemory).
    // `slot_of` floors a wider-than-slot argument (a 32-bit-ABI `double`, an
    // x86-64 `long double`) onto the slot its first byte occupies; the cursor
    // below turns these byte-position slots into argument ordinals.  `span` is
    // the largest slot any load anchored at a start slot reaches, so a wide
    // argument's two-slot footprint advances the cursor by two while its
    // ordinal advances by one.
    let mut groups: rustc_hash::FxHashMap<usize, Vec<NodeId>> = rustc_hash::FxHashMap::default();
    let mut span: rustc_hash::FxHashMap<usize, usize> = rustc_hash::FxHashMap::default();
    let mut disqualified: rustc_hash::FxHashSet<usize> = rustc_hash::FxHashSet::default();
    // One-shot scan: detection order doesn't matter (loads are grouped by
    // slot, then a cursor assigns ordinals), and the pass never re-enqueues,
    // so iterate the cached live Load set directly — no worklist, no RPO walk.
    let loads: Vec<NodeId> = ctx
        .live_of_kind(|k| matches!(k, NodeKind::Load(_)))
        .collect();
    for node_id in loads {
        let addr = ctx.load_addr(node_id);
        let [load_value] = ctx
            .node_outputs_exact::<1>(node_id)
            .expect("Load has 1 output per node signature");
        let Some(load_ty) = ctx.value_type_opt(load_value) else {
            continue;
        };
        let load_size = load_ty.byte_size() as i64;
        // (a) decompose to initial_sp + K.
        let Some(SpExpr { base, offset }) = SpDecomposer::new(ctx.function(), memo).decompose(addr)
        else {
            continue;
        };
        if base != initial_sp {
            continue;
        }
        // (b) the load's first byte falls in a stack slot.  Its last byte
        // (`offset + load_size - 1`) is at or above its first, so `slot_of`
        // is `Some`; `end_slot` is how far a wider-than-slot load reaches.
        let Some(start_slot) = stack_args.slot_of(offset) else {
            continue;
        };
        // The load's last byte is `offset + load_size - 1`.  A pathological
        // offset/size (from arbitrary lifted arithmetic) could overflow i64
        // here; treat an overflow as "not a stack arg" and skip rather than
        // panicking.
        let Some(last_byte) = offset.checked_add(load_size).and_then(|e| e.checked_sub(1)) else {
            continue;
        };
        let Some(end_slot) = stack_args.slot_of(last_byte) else {
            continue;
        };
        if disqualified.contains(&start_slot) {
            continue;
        }
        // (c) memory chain clean.  `mem_chain_is_dirty` re-derives the probe
        // (memory token / SP slot / width) from `node_id` — the SP decompose
        // is a memo hit from step (a).
        let dirty = mem_chain_is_dirty(ctx, node_id, alias_mode, mem_opts, memo, &mut shadow_memo);
        if dirty {
            disqualified.insert(start_slot);
            groups.remove(&start_slot);
            span.remove(&start_slot);
            continue;
        }
        groups.entry(start_slot).or_default().push(node_id);
        let reach = span.entry(start_slot).or_insert(start_slot);
        *reach = (*reach).max(end_slot);
    }

    // Width-aware cursor: walk byte-position slots from 0, assigning one
    // argument ordinal per anchored argument.  A wide argument consumes every
    // slot it spans (so the slots it covers are not mistaken for a gap), but
    // advances the ordinal by exactly one.  The first slot with no anchored
    // load (or a disqualified slot — those are absent from `groups`) ends the
    // contiguous prefix.
    let mut cursor = 0usize;
    let mut ordinal = first_stack_arg;
    while groups.contains_key(&cursor) {
        let arg_span = span[&cursor] - cursor + 1;
        let index = ordinal as u32;
        // Gather every qualifying load whose start slot falls inside this
        // argument's span: the anchor read plus any sub-field reads of the
        // same (possibly wider-than-one-slot) argument.
        let mut arg_loads: Vec<NodeId> = Vec::new();
        for s in cursor..cursor + arg_span {
            if let Some(loads) = groups.get(&s) {
                arg_loads.extend_from_slice(loads);
            }
        }
        // Same-space guard: one argument's carriers must share a single Load
        // space; a mismatch skips registration for this ordinal (the ordinal
        // is still consumed, mirroring the previous per-slot behaviour).
        let first_load = *arg_loads
            .first()
            .expect("a present span entry always has ≥1 anchored load");
        let NodeKind::Load(space) = *ctx.node_kind(first_load) else {
            unreachable!("group members are seeded from Load nodes");
        };
        if arg_loads
            .iter()
            .all(|&l| matches!(*ctx.node_kind(l), NodeKind::Load(s) if s == space))
        {
            for load in arg_loads {
                let [load_value] = ctx
                    .node_outputs_exact::<1>(load)
                    .expect("Load has 1 output per node signature");
                ctx.register_arg_value(index, load_value);
            }
        }
        cursor += arg_span;
        ordinal += 1;
    }
    Ok(())
}

/// One candidate stack-arg load's SP-rooted probe: the memory token it reads
/// through plus the `AddrClass::SpRooted { base, offset }` slot and its width.
/// Doubles as the [`ShadowMemo`] key — two candidates sharing the same
/// memory predecessor, SP base, slot, and width reuse the walk's verdict.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct LoadProbe {
    mem: ValueId,
    base: ValueId,
    offset: i64,
    load_size: i64,
}

/// Per-pass-call memo for [`mem_chain_is_dirty`], keyed by the [`LoadProbe`].
type ShadowMemo = rustc_hash::FxHashMap<LoadProbe, bool>;

/// Walks the memory chain backward from the load's memory input looking for any
/// def that may shadow its SP slot.  Returns `true` if any path through the
/// chain may overwrite bytes in the load's range.
///
/// Everything the probe needs — the memory token, the SP slot
/// (`AddrClass::SpRooted { base, offset }`), and the width — is derived from the
/// `load` node; the SP decompose is a memo hit (the sole caller already
/// decomposed the same address to qualify the load).  Delegates the traversal
/// (cycle-guarded, MemPhi-forking, stack-safe at any chain depth) to
/// [`SpAliasCfg::nearest_clobber`], whose per-def verdict comes from
/// `alias_mode` + the [`crate::MemAliasOptions`] relaxations.  Memoised per
/// pass-call on the derived [`LoadProbe`].
fn mem_chain_is_dirty(
    ctx: &mut crate::EditFunction<'_>,
    load: NodeId,
    alias_mode: crate::AliasMode,
    mem_opts: crate::MemAliasOptions,
    sp_memo: &mut SpExprMemo,
    memo: &mut ShadowMemo,
) -> bool {
    let [mem_token, addr] = ctx
        .graph_ref()
        .node_inputs_exact::<2>(load)
        .expect("Load has 2 inputs per node signature");
    let [load_value] = ctx
        .node_outputs_exact::<1>(load)
        .expect("Load has 1 output per node signature");
    let load_size = ctx
        .value_type_opt(load_value)
        .expect("Load output is a value")
        .byte_size() as i64;
    let SpExpr { base, offset } = SpDecomposer::new(ctx.function(), sp_memo)
        .decompose(addr)
        .expect("caller qualified this load: its address decomposes to SP + K");
    let probe = LoadProbe {
        mem: mem_token,
        base,
        offset,
        load_size,
    };
    if let Some(&cached) = memo.get(&probe) {
        return cached;
    }

    // The oracle uses the load only to narrow its memory edge onto the nearest
    // clobber; the slot range comes from the probe.  The chain is dirty iff that
    // nearest clobber is anything but the clean `InitialMemory` root.
    let clobber = SpAliasCfg::new(sp_memo, alias_mode, mem_opts).nearest_clobber(
        ctx,
        load,
        AddrClass::SpRooted { base, offset },
        load_size,
        mem_token,
    );
    let result = !matches!(ctx.node_kind(clobber), NodeKind::InitialMemory);
    memo.insert(probe, result);
    result
}

#[cfg(test)]
mod tests;
