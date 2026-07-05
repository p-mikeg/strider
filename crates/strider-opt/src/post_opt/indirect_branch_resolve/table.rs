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
//!    switch variable and its value-preserving derivations (`candidate_range`,
//!    collected over an anchor-first window then tried smallest-range-first so
//!    the tightly-bounded real index wins over any looser impostor).
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

/// How many nodes of the anchor-first cone scan to examine before giving up on
/// finding a table index.  A real dispatch index directly feeds the load's
/// address (`Load[base + idx*stride]`), so it sits only a handful of hops below
/// the dispatch value — measured at ≤ 8 even through width casts on a large x86
/// interpreter.  Beyond that we are in the index's *own* upstream (the
/// instruction-decode chain), where no candidate can make the dispatch fold —
/// and, crucially, where the per-node `value_range` query is far more expensive
/// (deep, complex expressions).  So a tight window is both sufficient and the
/// difference between a fast reject and a 9s one on the many indirect branches
/// that are NOT resolvable tables (function-pointer `call *[reg+off]`, etc.).
///
/// ponytail: fixed near-anchor scan window (64 ≈ 8× the observed max depth); it
/// fails SAFE — a table whose index is improbably deeper is reported unresolved,
/// never mis-resolved.  Raise it if a real dispatch is ever missed.
const MAX_INDEX_SCAN: usize = 64;

/// Largest pruned evaluation cone a candidate may have before it is rejected
/// without folding.  A real dispatch index's pruned cone is only the address
/// arithmetic plus the table load — a handful of nodes (observed ≤ 9), and
/// independent of the table's entry count.  A candidate with a large pruned
/// cone is deep in the index's own upstream: pinning it prunes almost nothing,
/// and folding that full cone COLD (rebuilding the SP-alias memo across
/// thousands of nodes) is what makes an unresolvable Load-dispatch cost seconds.
/// Building the cone is a cheap graph walk, so we build it, check its size, and
/// skip the fold when it is implausibly large.
///
/// ponytail: 128 ≈ 14× the observed max; well below the thousands-of-nodes
/// cones of the spurious deep candidates.  Fails SAFE (unresolved, not wrong).
const MAX_INDEX_CONE: usize = 128;

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

    // The dispatch is `f(index)` for one bounded `index`.  We don't pattern-
    // match the addressing; instead we scan the dispatch cone for bounded
    // candidate values and, for each, pin it to every value in its range and let
    // the abstract evaluator fold the dispatch.  The candidate that folds for
    // ALL its values IS the index, and the folded constants are the targets; a
    // wrong candidate leaves the dispatch dependent on the real (unseeded) index
    // and is rejected on its first fold.
    //
    // Candidate collection is ANCHOR-FIRST-windowed: walk the cone in reverse
    // postorder (dispatch value first) and take only the first `MAX_INDEX_SCAN`
    // nodes.  On a real dispatch the index sits a few hops below the load
    // (`Load[base + idx*stride]`), so it is in the window; the many bounded
    // values buried deep in the index's own upstream (its instruction-decode
    // chain, thousands of nodes) are never reached — and their per-node
    // `value_range` query (deep, complex expressions) is never paid.
    //
    // We then evaluate the collected candidates SMALLEST-RANGE-FIRST and return
    // the first that resolves.  This is load-bearing for correctness: a wider
    // candidate (e.g. a value_range-derived intermediate that spans more values
    // than the real switch variable) can fold to a plausible-looking run of
    // constants that are NOT valid targets — enumerating a table ENTRY instead
    // of the INDEX.  The real switch index is the most tightly bounded value, so
    // trying tightest-range first picks it before any looser impostor.  (Anchor
    // order alone does NOT guarantee this — the impostor can sit nearer the
    // anchor than the real index.)
    let order = super::eval::cone_order(ctx, anchor_value);
    let mut load_memo: rustc_hash::FxHashMap<ValueId, bool> = rustc_hash::FxHashMap::default();
    let mut candidates: Vec<(ValueId, u128, u128)> = order
        .iter()
        .rev()
        .take(MAX_INDEX_SCAN)
        .filter_map(|&v| candidate_range(ctx, v, anchor_value, branch, ranges, &mut load_memo))
        .collect();
    candidates.sort_by_key(|&(_, lo, hi)| hi - lo);

    let mut ev = super::eval::Evaluator::new(ctx, rom, alias_mode);
    for (idx_value, lo, hi) in candidates {
        // Prune the evaluation cone at the candidate: once it is pinned to a
        // concrete constant its own upstream producers are irrelevant, so each
        // per-index fold is O(index→dispatch path), not O(full cone).
        let pruned = super::eval::cone_order_pruned(ctx, anchor_value, idx_value);
        // A real dispatch index sits a few hops below the load, so its pruned
        // evaluation cone is tiny (a handful of nodes: the address arithmetic
        // and the table load).  A candidate whose pruned cone is large is deep
        // in the index's own upstream — pinning it prunes almost nothing, and
        // folding its full cone (cold, per fold) is what makes an unresolvable
        // Load-dispatch cost seconds.  Building the cone is a cheap graph walk;
        // skip the expensive fold when it is too big to be a real index.
        if pruned.len() > MAX_INDEX_CONE {
            continue;
        }
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

/// If `v` is a valid bounded index candidate for the dispatch anchored at
/// `anchor_value`, returns its inclusive `(value, lo, hi)` range; otherwise
/// `None`.  `value_range` only bounds the guard-/mask-constrained switch
/// variable and its `Add(X, const)` offset derivation, never a `Mul`-scaled
/// address term — so a returned candidate is contiguous over its range and safe
/// to enumerate.  (A guarded value wrapped in a width cast like
/// `ZeroExtend(Truncate(x))` resolves through the cast's inner operand, which
/// the caller's cone walk reaches directly, not via the outer cast.)
fn candidate_range(
    ctx: &strider_ir::Function,
    v: ValueId,
    anchor_value: ValueId,
    branch: NodeId,
    ranges: &mut crate::value_range::RangeMap<'_>,
    load_memo: &mut rustc_hash::FxHashMap<ValueId, bool>,
) -> Option<(ValueId, u128, u128)> {
    let ty = ctx.value_type_opt(v)?;
    if !ty.is_integer() {
        return None;
    }
    // Never enumerate the dispatch value ITSELF as the index.  A real table
    // dispatch reads/computes the target *from* a deeper index
    // (`Load[base + idx*stride]`), so the index is strictly inside the cone,
    // never the anchor.  Substituting the dispatch value directly with
    // `IntConst(i)` makes the branch's target literally `i` for every `i` in
    // the range — the identity-fold wrong-edge case.  Skip it.
    if v == anchor_value {
        return None;
    }
    // Skip constants — a literal operand (the `*2` scale, the table base) is
    // not the index.
    if ctx.int_const_u128(v).is_some() {
        return None;
    }
    // A loaded value is the table INDEX only when its range reflects dispatch
    // reachability — a range-check **guard** (e.g. a stack-passed arg
    // `Load[sp+K]` bounded by `cmp k,N`) or an explicit **mask** (`(kind & 7)`
    // on a stack-loaded arg).  A load-derived value with only a width-derived
    // KnownBits bound is a table ENTRY (e.g. a `tbb` byte, [0,255]);
    // enumerating its whole width folds to a run of bogus sequential targets.
    // So exclude load-derived values that are neither guard-bounded nor
    // mask-shaped.
    let entry_load = is_load_derived(ctx, v, load_memo)
        && ranges.dominating_guard(v, branch).is_none()
        && !is_and_masked(ctx, v);
    if entry_load {
        return None;
    }
    let iv = ranges.range_of(v, branch);
    let mask = ty.bit_mask_u128();
    // A finite range strictly inside the type's full width, capped to the
    // per-table enumeration limit.
    if iv.hi < mask && iv.hi >= iv.lo {
        let count = iv.hi - iv.lo + 1;
        if count <= u128::from(MAX_TABLE_ENTRIES) {
            return Some((v, iv.lo, iv.hi));
        }
    }
    None
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
