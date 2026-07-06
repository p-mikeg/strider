//! Table-dispatch arm of the indirect-branch classifier.
//!
//! A switch lowers to `branch f(index)` for one bounded `index` — whether the
//! table lives in rodata (`Load[ base + idx*stride ]`, absolute base), on the
//! stack (`Load[ (sp + K) + idx*stride ]`, SP-rooted), or behind a GP/GOT
//! indirection.  Rather than pattern-match each addressing shape, this arm
//! **delegates the addressing to the abstract evaluator**:
//!
//! 1. **Find candidate indices.**  Structurally decompose the dispatch's
//!    addressing (`decompose_indices`): walk only the address / index-derivation
//!    arithmetic (`+`, `*`, shifts, `&`, `|`, width casts) and, at a `Load`,
//!    into its address — collecting every genuinely-bounded [`crate::value_range`]
//!    value (a guard-/mask-constrained switch variable, never a width-only table
//!    entry).  This reaches the index inside `Load[base + idx*stride]`, the inner
//!    index of an offset table, and the index of a no-load computed jump, without
//!    scanning the whole backward cone — so there are no bounded-but-irrelevant
//!    impostors and a plain `Load[reg]` function pointer (no bounded index) is
//!    rejected outright.  Candidates are tried smallest-range-first so the
//!    tightly-bounded real index wins over any looser one.
//! 2. **Pin and fold.**  For each candidate, evaluate the dispatch cone under
//!    `index = i` via the read-only `super::eval::Evaluator` (ConstFold
//!    arithmetic + `LoadReadOnly` ROM reads + `LoadForward` via
//!    `reaching_store`) for every `i` in its proven range.  The dispatch value
//!    is a concrete constant iff the addressing fully resolved.  The evaluator
//!    covers *only* those three foldings — intentionally narrower than the full
//!    `default_pipeline` the former clone-and-optimise path ran.  A cone whose
//!    collapse to a constant would have required some other pass (e.g. a
//!    `KnownBits` bit-lattice narrowing) resolves to `None` here and the branch
//!    defers — sound (an unresolved branch is never a wrong edge), just less
//!    eager than the old approach.
//! 3. **Accept the index that folds every value.**  The candidate whose whole
//!    range folds to constants IS the index; the folded constants are the
//!    targets (`enumerate_targets`).  A wrong candidate leaves the dispatch
//!    dependent on the real index and fails to fold, so it is rejected.
//!
//! ## Soundness
//!
//! Two independent gates must hold to commit to `Multiple`:
//!
//! 1. **Bounded index.**  The dominator-scoped range analysis bounds `idx`
//!    from an `if (idx < N)` guard dominating the dispatch and/or a KnownBits
//!    mask (`idx & 0x7`).  A sound *upper* bound; mixed-bound joins fail closed.
//!    Only value-preserving derivations are bounded — never a `Mul`-scaled
//!    address term — so every candidate's range is contiguous and safe to
//!    enumerate.
//!
//! 2. **Complete fold.**  *Every* value in `lo..=hi` must fold to a constant
//!    target; any failure returns `None` (a `Multiple` omitting a real runtime
//!    target would wire a CFG missing edges).  The evaluator is read-only, so
//!    the analysed function is never mutated, and the caller's [`AliasMode`] is
//!    threaded into the evaluator so a global-clobbered on-stack table defers
//!    under `Strict` exactly as it would in the orchestrator's own run.
//!
//! Over-approximating the bound (extra targets) is sound — the surplus become
//! dead CFG edges.  Under-approximating is not.  Failing either gate returns
//! `None` and the orchestrator defers the branch (ultimately
//! `UnresolvedIndirectBranch` at fixed point).  No panic, no partial
//! commitment.

#![allow(clippy::module_name_repetitions)]

use super::MAX_TABLE_ENTRIES;
use crate::{AliasMode, ReadOnlyMemory};
use strider_cfg::ResolvedTargets;
use strider_ir::IRViewer;
use strider_ir::node::{IntBinaryOp, NodeId, NodeKind, ValueId};

/// Top-level classifier hook for the table-dispatch arm.  Called by
/// [`super::classify_anchor`] when the anchor's producer is a
/// [`NodeKind::Load`] or an `IntBinaryOp(And)` dispatch-mask wrapper.
///
/// `rom` is the binary's read-only image (rodata/text); `None` disables the
/// absolute (rodata) arm.  The stack-pointer varnode (for the SP-rooted arm)
/// and the target endianness (for the rodata read) are read off `ctx` —
/// `ctx.default_cc().stack_vn` and `ctx.endianness()`.
#[must_use]
pub fn classify_table_dispatch(
    ctx: &strider_ir::Function,
    branch: NodeId,
    rom: Option<&dyn ReadOnlyMemory>,
    ranges: &mut crate::value_range::RangeMap<'_>,
    alias_mode: AliasMode,
) -> Option<ResolvedTargets> {
    // The `IndirectBranch` placeholder's slot-2 input ([control, memory,
    // target]) is its current dispatch value — the anchor we analyse.  Taking
    // the branch NODE (not the bare value) means the index-range query below is
    // scoped to the branch ACTUALLY being resolved, never the first
    // `IndirectBranch` that happens to share the dispatch value.
    let anchor_value = ctx.indirect_branch_target(branch);

    // Structurally decompose the dispatch's addressing to THE index: the
    // shallowest genuinely-bounded (guard- or mask-constrained, never
    // width-only) value inside the target/address arithmetic — reached by
    // walking only the addressing ops (`Add`/`Mul`/`Shl`/`And`/`Or`/casts) and
    // into load addresses.  Because it touches only the addressing (not the
    // whole backward cone) there are no bounded-but-irrelevant impostors and no
    // deep decode-chain values to bound with a scan/cone knob; a plain
    // `Load[reg]` function pointer has no such index and is rejected here with
    // no fold at all.
    let candidates = decompose_indices(ctx, ranges, anchor_value, branch);

    // Pin each candidate index over its proven range and let the read-only
    // evaluator fold the index-pruned dispatch cone for every value: rodata
    // reads via `LoadReadOnly`, on-stack reads via `reaching_store`, arithmetic
    // via ConstFold.  Every value must fold to a constant target (fail-closed):
    // a dispatch whose base is symbolic — e.g. a vtable `Load[reg + idx*8]` —
    // does not fold and the branch defers.  No size guard on the cone is needed:
    // the evaluator identifies the SP spine structurally (no per-node cone walk),
    // so even a false-positive candidate with a large decode cone folds cheaply
    // and `enumerate_targets` bails on its first non-folding value.
    //
    // Candidates are tried SMALLEST-RANGE-FIRST (`decompose_indices` sorts
    // them): when the addressing exposes more than one bounded origin — a wide
    // mask around the tightly-guarded switch variable — the real index is the
    // most tightly bounded one, so it wins before any looser impostor that
    // would fold to a run of bogus sequential targets.
    let mut ev = super::eval::Evaluator::new(ctx, rom, alias_mode);
    for (idx_value, lo, hi) in candidates {
        let pruned = super::eval::cone_order_pruned(ctx, anchor_value, idx_value);
        if let Some(targets) =
            enumerate_targets(lo, hi, |x| ev.eval_target(&pruned, anchor_value, idx_value, x))
        {
            return Some(ResolvedTargets::Multiple(targets));
        }
    }
    None
}

/// Enumerate the table by folding the dispatch for every value in `lo..=hi`.
/// Returns the sorted-deduplicated targets, or `None` if ANY value fails to
/// fold (this candidate is not the index, or the table is not fully resolvable
/// → fail closed).  `fold` does the per-value substitution-and-optimise.
fn enumerate_targets(
    lo: u128,
    hi: u128,
    mut fold: impl FnMut(u128) -> Option<u64>,
) -> Option<Vec<u64>> {
    let mut targets: Vec<u64> = (lo..=hi).map(&mut fold).collect::<Option<_>>()?;
    targets.sort_unstable();
    targets.dedup();
    (!targets.is_empty()).then_some(targets)
}

/// The dispatch index candidates: every **genuinely-bounded** (guard- or
/// mask-constrained — never width-only) non-constant value reachable from the
/// anchor through the target/address arithmetic and into load addresses,
/// **smallest-range-first**.
///
/// Walks only the addressing / index-derivation ops (`Add`/`Mul`/shifts/`And`/
/// `Or`, `Extend`/`Truncate`) and, at a `Load`, its address — so it reaches the
/// index inside `Load[base + idx*stride]`, the inner index of an offset table
/// `Load[offtable + idx]`, and the index of a no-load computed jump
/// `(base + idx<<k) & mask`.  A `Load`'s *output* is a table entry, never
/// followed, so the walk cannot wander into an index loaded from memory; a
/// visited-set keeps it linear over the (shared-DAG) arithmetic it does follow.
/// A plain `Load[reg]` function pointer exposes no bounded index → empty.
///
/// Following `>>` reaches a legitimately shifted index but can also step into
/// the instruction-decode chain, so this over-collects: some candidates are
/// false positives deep in decode.  Two things make that safe and cheap —
/// smallest-range-first (the tightly-bounded real index is tried before any
/// looser one, which would fold to bogus sequential targets), and the caller's
/// per-candidate fold-cone guard (a false positive has a large fold cone and is
/// rejected without folding).
fn decompose_indices(
    ctx: &strider_ir::Function,
    ranges: &mut crate::value_range::RangeMap<'_>,
    anchor: ValueId,
    branch: NodeId,
) -> Vec<(ValueId, u128, u128)> {
    let mut out = Vec::new();
    let mut load_memo = rustc_hash::FxHashMap::default();
    let mut seen = rustc_hash::FxHashSet::default();
    collect_indices(
        ctx, ranges, anchor, anchor, branch, &mut out, &mut load_memo, &mut seen, false,
    );
    out.sort_by_key(|&(_, lo, hi)| hi - lo);
    out
}

#[allow(clippy::too_many_arguments)]
fn collect_indices(
    ctx: &strider_ir::Function,
    ranges: &mut crate::value_range::RangeMap<'_>,
    v: ValueId,
    anchor: ValueId,
    branch: NodeId,
    out: &mut Vec<(ValueId, u128, u128)>,
    load_memo: &mut rustc_hash::FxHashMap<ValueId, bool>,
    seen: &mut rustc_hash::FxHashSet<ValueId>,
    // True once the walk is inside an INDEX position: the non-base operand of an
    // `Add(base, idx·scale)` whose `base` is a constant (rodata) or SP-rooted
    // (stack) address.  A value reached only outside such a position — e.g. the
    // `reg` in a `Load[reg + idx*8]` vtable / `Load[reg]` function pointer — is
    // NOT a dispatch index and is never collected (so those defer with no fold).
    in_index_pos: bool,
) {
    // Visit each value once — the addressing / decode graph is a shared DAG, so
    // a naive DFS would re-walk shared subgraphs combinatorially.
    if !seen.insert(v) {
        return;
    }
    // The anchor itself is never the index (substituting it makes the target
    // literally the index — the identity-fold wrong edge); constants are the
    // base / stride, not the index.
    if let Some(ty) = ctx.value_type_opt(v).filter(|t| {
        in_index_pos && t.is_integer() && v != anchor && ctx.int_const_u128(v).is_none()
    }) {
        let iv = ranges.range_of(v, branch);
        // A finite range strictly inside the type width, within the enumeration
        // cap.
        let bounded = iv.hi >= iv.lo
            && iv.hi < ty.bit_mask_u128()
            && iv.hi - iv.lo < u128::from(MAX_TABLE_ENTRIES);
        // Exclude a loaded table ENTRY: a load-derived value bounded only by its
        // load width (`ZeroExtend(Load.byte)` spanning [0,255]) whose range
        // reflects no dispatch reachability — no dominating **guard**
        // (`if idx < N`) and no explicit **mask** (`idx & 7`).  Enumerating an
        // entry folds to bogus sequential targets.  Everything else finite is a
        // candidate index.
        let entry_load = is_load_derived(ctx, v, load_memo)
            && ranges.dominating_guard(v, branch).is_none()
            && !is_and_masked(ctx, v);
        if bounded && !entry_load {
            out.push((v, iv.lo, iv.hi));
        }
    }
    // Recurse through the addressing / index-derivation arithmetic (`+`, `*`,
    // shifts, `&`, `|`, width casts) and, at a `Load`, into its address — so the
    // index is reached however the compiler scaled and sliced it, including a
    // `>>` field-extract.  We deliberately do NOT follow the full integer
    // vocabulary (`Xor`, `IntUnaryOp`, …): those pull the walk deep into the
    // instruction-decode chain, whose bounded sub-values would flood the
    // candidate set.  We stop at a `Load`'s *value* (its output is a table
    // ENTRY — its address is followed instead); the visited-set keeps the walk
    // linear over the shared DAG.
    let node = ctx.producer(v);
    // At an `Add(base, other)` with a const / SP-rooted `base`, the `other`
    // operand enters INDEX position (`base + idx·scale`); the base operand does
    // not.  Every other addressing op just carries the current position to its
    // inputs.  This is the `base + idx·scale`, `base ∈ {const, sp}` pattern.
    let inputs: Vec<ValueId> = ctx
        .node_inputs(node)
        .into_iter()
        .filter(|&i| ctx.value_type_opt(i).is_some_and(|t| t.is_integer()))
        .collect();
    let indexing_add = matches!(ctx.node_kind(node), NodeKind::IntBinaryOp(IntBinaryOp::Add))
        && inputs.len() == 2
        && (is_base_operand(ctx, inputs[0]) || is_base_operand(ctx, inputs[1]));
    let follow = indexing_add
        || matches!(
            ctx.node_kind(node),
            NodeKind::Load(_)
                | NodeKind::Extend(_)
                | NodeKind::Truncate
                | NodeKind::IntBinaryOp(
                    IntBinaryOp::Add
                        | IntBinaryOp::Mul
                        | IntBinaryOp::ShiftLeft
                        | IntBinaryOp::ShiftRight
                        | IntBinaryOp::SShiftRight
                        | IntBinaryOp::And
                        | IntBinaryOp::Or,
                )
        );
    if !follow {
        return;
    }
    for i in inputs {
        // An indexing `Add`'s base operand stays out of index position; its
        // other operand enters it.  All other ops propagate the current flag.
        let child_pos = if indexing_add {
            !is_base_operand(ctx, i)
        } else {
            in_index_pos
        };
        collect_indices(ctx, ranges, i, anchor, branch, out, load_memo, seen, child_pos);
    }
}

/// A `base` operand of an indexing `Add(base, idx·scale)`: a constant address
/// (rodata table base) or an SP-rooted address (stack table).  On the converged
/// graph a foldable base is already a literal `IntConst`; a register / GOT base
/// (function pointer, vtable, PIC) is neither, so its indexed operand never
/// enters index position and is not collected.
fn is_base_operand(ctx: &strider_ir::Function, v: ValueId) -> bool {
    ctx.int_const_u128(v).is_some() || is_sp_rooted(ctx, v, 8)
}

/// Is `v` an SP-rooted address — `InitialVar(sp)`, `Add(sp-rooted, const)`, or
/// the alignment base `And(sp-rooted, mask)` — checked structurally with a small
/// depth bound (the SP spine is a short const-offset chain).
fn is_sp_rooted(ctx: &strider_ir::Function, v: ValueId, depth: u32) -> bool {
    if depth == 0 {
        return false;
    }
    let node = ctx.producer(v);
    match ctx.node_kind(node) {
        NodeKind::InitialVar(id) => ctx.initial_vn(*id) == ctx.default_cc().stack_vn,
        NodeKind::IntBinaryOp(IntBinaryOp::Add | IntBinaryOp::And) => ctx
            .node_inputs(node)
            .into_iter()
            .any(|i| ctx.value_type_opt(i).is_some() && is_sp_rooted(ctx, i, depth - 1)),
        _ => false,
    }
}

/// Is `v` (transitively) the output of a `Load`?  A loaded value is a table
/// *entry*, not the table *index*, so it must not be enumerated as one.
fn is_load_derived(
    ctx: &strider_ir::Function,
    v: ValueId,
    memo: &mut rustc_hash::FxHashMap<ValueId, bool>,
) -> bool {
    if let Some(&cached) = memo.get(&v) {
        return cached;
    }
    // Pre-seed `false` to break cycles (a value Phi can be self-referential).
    memo.insert(v, false);
    let node = ctx.producer(v);
    let result = matches!(ctx.node_kind(node), NodeKind::Load(_))
        || ctx
            .node_inputs(node)
            .into_iter()
            .any(|input| ctx.value_type_opt(input).is_some() && is_load_derived(ctx, input, memo));
    memo.insert(v, result);
    result
}

/// Is `v` produced by an `And(_, IntConst)`?  Such a value is mask-bounded
/// (e.g. `kind & 7`), a legitimate dispatch index even when the masked operand
/// was loaded — unlike a table entry, whose bound is only its load width.
fn is_and_masked(ctx: &strider_ir::Function, v: ValueId) -> bool {
    let node = ctx.producer(v);
    matches!(ctx.node_kind(node), NodeKind::IntBinaryOp(IntBinaryOp::And))
        && ctx
            .node_inputs(node)
            .into_iter()
            .any(|input| ctx.int_const_u128(input).is_some())
}

#[cfg(test)]
#[path = "table_tests.rs"]
mod table_tests;
