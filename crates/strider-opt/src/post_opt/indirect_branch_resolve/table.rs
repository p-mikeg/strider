//! Table-dispatch arm of the indirect-branch classifier.
//!
//! A switch lowers to `branch f(index)` for one bounded `index` — whether the
//! table lives in rodata (`Load[ base + idx*stride ]`, absolute base), on the
//! stack (`Load[ (sp + K) + idx*stride ]`, SP-rooted), or behind a GP/GOT
//! indirection.  Rather than pattern-match each addressing shape, this arm
//! **delegates the addressing to the abstract evaluator**:
//!
//! 1. **Find candidate indices.**  Walk the dispatch cone for integer values
//!    with a finite [`crate::value_range`] bound — the guard-/mask-constrained
//!    switch variable and its value-preserving derivations
//!    ([`find_index_candidates`]).
//! 2. **Pin and fold.**  For each candidate, evaluate the dispatch cone under
//!    `index = i` via the read-only [`super::eval::Evaluator`] (ConstFold
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
//!    targets ([`enumerate_targets`]).  A wrong candidate leaves the dispatch
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
use crate::AliasMode;
use crate::ReadOnlyMemory;
use strider_cfg::ResolvedTargets;
use strider_ir::IRViewer;
use strider_ir::node::{IntBinaryOp, NodeId, NodeKind, ValueId};

/// Top-level classifier hook for the table-dispatch arm.  Called by
/// [`super::classify_anchor`] when the anchor's producer is a
/// [`NodeKind::Load`] or an `IntBinaryOp(And)` dispatch-mask wrapper.
///
/// `anchor_value` is the placeholder `IndirectBranch`'s dispatch-value
/// input.  `rom` is the binary's read-only image (rodata/text); `None`
/// disables the absolute (rodata) arm.  The stack-pointer varnode (for the
/// SP-rooted arm) and the target endianness (for the rodata read) are read
/// off `ctx` — `ctx.default_cc().stack_vn` and `ctx.endianness()`.
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

    // The dispatch is `f(index)` for one bounded `index`.  We don't pattern-
    // match the addressing; instead we find candidate bounded values in the
    // dispatch cone, and for each, pin it to every value in its range and let
    // the real optimiser fold the dispatch.  The candidate that folds for ALL
    // its values IS the index, and the folded constants are the targets.  A
    // wrong candidate fails to fold (the dispatch still depends on the real
    // index) and is rejected after one or two tries.
    let candidates = find_index_candidates(ctx, anchor_value, branch, ranges);
    if candidates.is_empty() {
        return None;
    }

    // Evaluate the dispatch cone under each concrete index — no clone, no
    // pipeline. The cone + its topological order are index-independent, so
    // build them once and reuse across candidates and indices. The candidate
    // whose whole range collapses to constants IS the index; the constants are
    // the targets. A wrong candidate leaves the cone dependent on a non-seeded
    // runtime value and fails to collapse → rejected.
    let order = super::eval::cone_order(ctx, anchor_value);
    let mut ev = super::eval::Evaluator::new(ctx, rom, alias_mode);
    for (idx_value, lo, hi) in candidates {
        if let Some(targets) =
            enumerate_targets(lo, hi, |v| ev.eval_target(&order, anchor_value, idx_value, v))
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
    let mut targets: Vec<u64> = Vec::new();
    for v in lo..=hi {
        targets.push(fold(v)?);
    }
    targets.sort_unstable();
    targets.dedup();
    (!targets.is_empty()).then_some(targets)
}

/// Candidate index values: every integer value reachable from `anchor_value`
/// with a finite range, as `(value, lo, hi)` (inclusive), smallest range
/// first.  `value_range` only bounds the guard-/mask-constrained switch
/// variable and its `Add(X, const)` offset derivation, never a `Mul`-scaled
/// address term — so every candidate is contiguous over its range and safe to
/// enumerate.  (A guarded value wrapped in a width cast like
/// `ZeroExtend(Truncate(x))` resolves through the cast's inner operand, which
/// this cone walk reaches directly, not via the outer cast.)
fn find_index_candidates(
    ctx: &strider_ir::Function,
    anchor_value: ValueId,
    branch: NodeId,
    ranges: &mut crate::value_range::RangeMap<'_>,
) -> Vec<(ValueId, u128, u128)> {
    let mut out: Vec<(ValueId, u128, u128)> = Vec::new();
    let mut seen: rustc_hash::FxHashSet<ValueId> = rustc_hash::FxHashSet::default();
    let mut load_memo: rustc_hash::FxHashMap<ValueId, bool> = rustc_hash::FxHashMap::default();
    let mut stack = vec![anchor_value];
    while let Some(v) = stack.pop() {
        if !seen.insert(v) {
            continue;
        }
        if let Some(ty) = ctx.value_type_opt(v) {
            // Never enumerate the dispatch value ITSELF as the index.  A real
            // table dispatch reads/computes the target *from* a deeper index
            // (`Load[base + idx*stride]` / `Load[(sp+K) + idx*stride]`), so
            // the index is strictly inside the cone, never the anchor.
            // Substituting the dispatch value directly with `IntConst(i)` makes
            // the branch's target literally `i` for every `i` in the range —
            // folding a guarded direct-load (or direct-mask) anchor into bogus
            // sequential targets equal to the bare index values.  This is the
            // identity-fold wrong-edge case: skip it.
            let is_anchor = v == anchor_value;
            // Skip constants — a literal operand (the `*2` scale, the table
            // base) is not the index.
            let is_const = ctx.int_const_u128(v).is_some();
            // A loaded value is the table INDEX only when its range reflects
            // dispatch reachability — a range-check **guard** (e.g. a
            // stack-passed arg `Load[sp+K]` bounded by `cmp k,N`) or an explicit
            // **mask** (`(kind & 7)` on a stack-loaded arg).  A load-derived
            // value with only a width-derived KnownBits bound is a table ENTRY
            // (e.g. a `tbb` byte, [0,255]); enumerating its whole width folds to
            // a run of bogus sequential targets.  So exclude load-derived values
            // that are neither guard-bounded nor mask-shaped.
            let entry_load = is_load_derived(ctx, v, &mut load_memo)
                && ranges.dominating_guard(v, branch).is_none()
                && !is_and_masked(ctx, v);
            if ty.is_integer() && !is_anchor && !is_const && !entry_load {
                let iv = ranges.range_of(v, branch);
                let mask = ty.bit_mask_u128();
                // A finite range strictly inside the type's full width, capped
                // to the per-table enumeration limit.
                if iv.hi < mask && iv.hi >= iv.lo {
                    let count = iv.hi - iv.lo + 1;
                    if count <= u128::from(MAX_TABLE_ENTRIES) {
                        out.push((v, iv.lo, iv.hi));
                    }
                }
            }
        }
        for input in ctx.node_inputs(ctx.producer(v)) {
            if ctx.value_type_opt(input).is_some() {
                stack.push(input);
            }
        }
    }
    // Try the tightest-bounded candidates first: a wrong one fails fast, and the
    // real index is usually the narrowest finite range in the cone.
    out.sort_by_key(|&(_, lo, hi)| hi - lo);
    out
}

/// Is `v` produced by an `And(_, IntConst)`?  Such a value is mask-bounded
/// (e.g. `kind & 7`), which makes it a legitimate index even when the masked
/// operand was loaded — unlike a table entry, whose bound is its load width.
fn is_and_masked(ctx: &strider_ir::Function, v: ValueId) -> bool {
    let node = ctx.producer(v);
    matches!(ctx.node_kind(node), NodeKind::IntBinaryOp(IntBinaryOp::And))
        && ctx
            .node_inputs(node)
            .into_iter()
            .any(|input| ctx.int_const_u128(input).is_some())
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

#[cfg(test)]
#[path = "table_tests.rs"]
mod table_tests;
