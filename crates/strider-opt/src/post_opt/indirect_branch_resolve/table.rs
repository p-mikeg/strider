//! Table-dispatch arm of the indirect-branch classifier.
//!
//! A switch lowers to `branch f(index)` for one bounded `index` — whether the
//! table lives in rodata (`Load[ base + idx*stride ]`, absolute base), on the
//! stack (`Load[ (sp + K) + idx*stride ]`, SP-rooted), or behind a GP/GOT
//! indirection.  Rather than pattern-match each addressing shape, this arm
//! **delegates the addressing to the abstract evaluator**:
//!
//! 1. **Find candidate indices.**  Collect the bounded values in the dispatch's
//!    **variability cone** (`decompose_indices`): the values derived from THE one
//!    variable that controls the branch, reached by walking the target's integer
//!    ancestors and stopping at loads / opaque sources (recursing into a load's
//!    *address* when the index sits behind it).  This reaches the index inside
//!    `Load[base + idx*stride]`, the inner index of an offset table, and the
//!    index of a no-load computed jump alike.  A plain `Load[reg]` function
//!    pointer (no bounded value in its cone) is rejected outright.  Candidates
//!    are tried smallest-range-first so the tightly-bounded real index wins over
//!    a looser derived form (a `Mul`-scaled address term whose dense range would
//!    fold to garbage).
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
//! 1. **Bounded index.**  The range analysis bounds `idx` from an `if (idx < N)`
//!    guard dominating the dispatch and/or a KnownBits mask (`idx & 0x7`).  A
//!    sound *upper* bound; mixed-bound joins fail closed.  A `Mul`-scaled address
//!    term has a dense range that would fold to garbage, but smallest-range-first
//!    tries the tightest (value-preserving) derivation before any scaled one.
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
use crate::value_range::Interval;
use crate::{AliasMode, ReadOnlyMemory};
use strider_cfg::ResolvedTargets;
use strider_ir::IRViewer;
use strider_ir::node::{IntBinaryOp, NodeId, NodeKind, ValueId};

/// Top-level classifier hook for the table-dispatch arm.  Called by
/// [`super::classify_target`] when the target's producer is a
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
    // target]) is its current dispatch value — the target we analyse.  Taking
    // the branch NODE (not the bare value) means the index-range query below is
    // scoped to the branch ACTUALLY being resolved, never the first
    // `IndirectBranch` that happens to share the dispatch value.
    let target_value = ctx.indirect_branch_target(branch);

    // Collect candidate indices from the dispatch's single variability cone
    // (`decompose_indices`): the bounded values derived from THE one variable
    // that controls the branch.  A `Load[reg]` function pointer has no bounded
    // value in its cone → empty → deferred with no fold.
    let candidates = decompose_indices(ctx, ranges, target_value, branch);

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
    for (idx_value, range) in candidates {
        let pruned = super::eval::cone_order_pruned(ctx, target_value, idx_value);
        if let Some(targets) =
            enumerate_targets(range, |x| ev.eval_target(&pruned, target_value, idx_value, x))
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
    range: Interval,
    mut fold: impl FnMut(u128) -> Option<u64>,
) -> Option<Vec<u64>> {
    let mut targets: Vec<u64> = (range.lo..=range.hi).map(&mut fold).collect::<Option<_>>()?;
    targets.sort_unstable();
    targets.dedup();
    (!targets.is_empty()).then_some(targets)
}

/// The dispatch index candidates: the **genuinely-bounded** (guard- or
/// mask-constrained, never width-only) non-constant values that **dominate the
/// target** in its variability cone, **smallest-range-first**.
///
/// A jump table is `branch f(index)` for one controlling variable, so the index
/// is a node **every** variable→target path flows through — a *value-dominator*
/// of the target.  Restricting to dominators is what excludes a bypassed
/// sub-branch: in a rotate `index = (x<<2) | (x>>30)`, `x>>30` has the tightest
/// interval but does **not** dominate the target (the `x<<2` arm bypasses it), so
/// pinning it leaves `x` free and it can never fold; `x` and the `Or` *do*
/// dominate.  Smallest-range over the raw cone would try `x>>30` first; smallest
/// range over the **dominators** never offers it.
///
/// The cone is built by walking `target`'s variability inputs, TRAVERSING THROUGH
/// a load the evaluator can fold (const-base rodata / SP-rooted stack) into its
/// address and stopping at a reg/GOT-based load (vtable / funcptr / PIC) and at
/// opaque sources.  A virtual ENTRY feeds every cone root, and
/// [`petgraph::algo::dominators::simple_fast`] yields the target's dominator
/// chain — the short "convergence → target" spine, not the whole cone.  The pin
/// is still confirmed by the caller's fold (belt-and-braces), but a valid table
/// now resolves on the first candidate.
fn decompose_indices(
    ctx: &strider_ir::Function,
    ranges: &mut crate::value_range::RangeMap<'_>,
    target: ValueId,
    branch: NodeId,
) -> Vec<(ValueId, Interval)> {
    use petgraph::graph::{DiGraph, NodeIndex};

    // Build the value-dominance graph of the cone: a virtual ENTRY with an edge
    // to every root, and a producer→consumer edge for every variability edge
    // (traversing through a foldable load into its address).  Node weight
    // `None` marks the ENTRY; `Some(v)` a cone value.
    let mut g: DiGraph<Option<ValueId>, ()> = DiGraph::new();
    let entry = g.add_node(None);
    let mut nidx: rustc_hash::FxHashMap<ValueId, NodeIndex> = rustc_hash::FxHashMap::default();
    let node_of = |g: &mut DiGraph<Option<ValueId>, ()>,
                       nidx: &mut rustc_hash::FxHashMap<ValueId, NodeIndex>,
                       v: ValueId| {
        *nidx.entry(v).or_insert_with(|| g.add_node(Some(v)))
    };

    let mut seen: rustc_hash::FxHashSet<ValueId> = rustc_hash::FxHashSet::default();
    let mut stack = vec![target];
    while let Some(v) = stack.pop() {
        if ctx.int_const_u128(v).is_some() || !seen.insert(v) {
            continue;
        }
        let vi = node_of(&mut g, &mut nidx, v);
        // `v`'s variability inputs: the addressing arithmetic, a foldable load's
        // address, or nothing for an opaque source.
        let inputs: Vec<ValueId> = match ctx.node_kind(ctx.producer(v)) {
            NodeKind::Load(_) => foldable_load_address(ctx, v).into_iter().collect(),
            NodeKind::InitialVar(_)
            | NodeKind::Phi
            | NodeKind::Call
            | NodeKind::CallOther { .. }
            | NodeKind::New
            | NodeKind::SegmentOp { .. }
            | NodeKind::CPoolRef => Vec::new(),
            _ => int_inputs(ctx, v),
        };
        let mut has_var_input = false;
        for p in inputs {
            // A const or a PURE SP-spine value (`sp`, `sp+K`, `(sp+K)&mask`) is a
            // symbolic *base*, not a variable: the evaluator keeps it as `SpRel`
            // and never enumerates it.  Skipping it (like a const) keeps `sp` from
            // being a second root — otherwise the real index fails to dominate the
            // target (the SP path bypasses it) and every stack table would defer.
            if ctx.int_const_u128(p).is_some() || is_pure_sp_base(ctx, p, 8) {
                continue;
            }
            has_var_input = true;
            let pi = node_of(&mut g, &mut nidx, p);
            g.add_edge(pi, vi, ());
            stack.push(p);
        }
        if !has_var_input {
            g.add_edge(entry, vi, ()); // a root of the variability cone
        }
    }

    let Some(&target_idx) = nidx.get(&target) else {
        return Vec::new();
    };
    let doms = petgraph::algo::dominators::simple_fast(&g, entry);

    // The target's dominator chain (excluding ENTRY and the target itself), kept
    // when a genuinely-bounded, non-entry index — smallest-range-first.
    let mut load_memo = rustc_hash::FxHashMap::default();
    let mut out: Vec<(ValueId, Interval)> = Vec::new();
    if let Some(chain) = doms.dominators(target_idx) {
        for di in chain {
            let Some(v) = *g.node_weight(di).expect("dominator is a graph node") else {
                continue; // ENTRY
            };
            if v == target {
                continue;
            }
            if let Some(c) = bounded_index(ctx, ranges, branch, v, &mut load_memo) {
                out.push(c);
            }
        }
    }
    out.sort_by_key(|&(_, iv)| iv.hi - iv.lo);
    out
}

/// The address of a load the abstract evaluator can fold -- one whose address is
/// (or has an operand that is) a const (rodata, resolved by `LoadReadOnly`) or an
/// SP-rooted address (stack, resolved by `reaching_store`) -- or `None` for a
/// reg / GOT-based load (vtable / funcptr / PIC) it cannot.  Traversing into a
/// foldable load's address continues the index search past a table's entry load;
/// stopping at an unfoldable one lets the branch defer without chasing a
/// non-existent index.
///
/// SP-relativeness is checked structurally, NOT via `stack_offsets`: that
/// side-table records the *fixed* SP offset of scalar spills/slots, but a stack
/// *table* load `Load[(sp+base) + idx*stride]` has an **index-dependent** offset
/// `StackOffsetDetect` never tags, so the side-table returns `None` for exactly
/// the loads we need to traverse.
fn foldable_load_address(ctx: &strider_ir::Function, load: ValueId) -> Option<ValueId> {
    let addr = int_inputs(ctx, load).first().copied()?;
    let foldable = is_base_operand(ctx, addr)
        || int_inputs(ctx, addr)
            .iter()
            .any(|&op| is_base_operand(ctx, op));
    foldable.then_some(addr)
}

/// A const address (rodata table base) or an SP-rooted address (stack table
/// base) -- the two bases the evaluator can fold a `Load` through.
fn is_base_operand(ctx: &strider_ir::Function, v: ValueId) -> bool {
    ctx.int_const_u128(v).is_some() || is_sp_rooted(ctx, v, 8)
}

/// Is `v` an SP-rooted address -- `InitialVar(sp)`, or an `Add`/`And` with **any**
/// SP-rooted operand -- checked structurally with a small depth bound.  Used by
/// [`foldable_load_address`]: a load whose address touches SP *anywhere* is
/// foldable (the evaluator reads it via `reaching_store`), even a table load
/// `Load[(sp+base) + idx*stride]` whose address also carries the index.
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

/// Is `v` a **pure** SP-spine base -- `InitialVar(sp)`, `Add(sp-spine, const)`, or
/// `And(sp-spine, const-mask)` -- i.e. SP offset by *constants only*, no index
/// term.  Unlike [`is_sp_rooted`], `Add((sp+base), idx*stride)` is NOT a pure SP
/// base (its non-SP operand is a variable), so the index it carries is still
/// walked.  The dominance walk treats a pure SP base like a const: a symbolic
/// frame anchor, never enumerated, never a variability root.
fn is_pure_sp_base(ctx: &strider_ir::Function, v: ValueId, depth: u32) -> bool {
    if depth == 0 {
        return false;
    }
    let node = ctx.producer(v);
    match ctx.node_kind(node) {
        NodeKind::InitialVar(id) => ctx.initial_vn(*id) == ctx.default_cc().stack_vn,
        NodeKind::IntBinaryOp(IntBinaryOp::Add | IntBinaryOp::And) => {
            let ins = int_inputs(ctx, v);
            ins.len() == 2
                && ins.iter().any(|&i| is_pure_sp_base(ctx, i, depth - 1))
                && ins.iter().any(|&i| ctx.int_const_u128(i).is_some())
        }
        _ => false,
    }
}

/// `v` as a candidate index: a genuinely-bounded (guard-/mask-constrained, never
/// width-only) non-constant integer.  Excludes a loaded table *entry* -- a
/// load-derived value bounded only by its load width (`ZeroExtend(Load.byte)`
/// spanning `[0,255]`) with no dominating guard and no explicit mask --
/// enumerating one folds to bogus sequential targets.
fn bounded_index(
    ctx: &strider_ir::Function,
    ranges: &mut crate::value_range::RangeMap<'_>,
    branch: NodeId,
    v: ValueId,
    load_memo: &mut rustc_hash::FxHashMap<ValueId, bool>,
) -> Option<(ValueId, Interval)> {
    let ty = ctx
        .value_type_opt(v)
        .filter(|t| t.is_integer() && ctx.int_const_u128(v).is_none())?;
    let iv = ranges.range_of(v, branch);
    let bounded = iv.hi >= iv.lo
        && iv.hi < ty.bit_mask_u128()
        && iv.hi - iv.lo < u128::from(MAX_TABLE_ENTRIES);
    let entry_load = is_load_derived(ctx, v, load_memo)
        && ranges.dominating_guard(v, branch).is_none()
        && !is_and_masked(ctx, v);
    (bounded && !entry_load).then_some((v, iv))
}


/// A node's integer-typed value inputs (the index-bearing dataflow edges).
fn int_inputs(ctx: &strider_ir::Function, v: ValueId) -> Vec<ValueId> {
    ctx.node_inputs(ctx.producer(v))
        .into_iter()
        .filter(|&i| ctx.value_type_opt(i).is_some_and(|t| t.is_integer()))
        .collect()
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
