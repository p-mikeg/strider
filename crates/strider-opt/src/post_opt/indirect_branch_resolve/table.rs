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
/// and the target endianness (for the rodata read) are read off `function` —
/// `function.default_cc().stack_vn` and `function.endianness()`.
#[must_use]
pub fn classify_table_dispatch(
    function: &strider_ir::Function,
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
    let target_value = function.indirect_branch_target(branch);

    // Find THE index: the deepest value on the dispatch's variability cone that
    // both dominates the target and carries a bounded strided range
    // (`decompose_index`).  Dominance rules out structural impostors (a rotate's
    // `x>>30` has a tight range but doesn't dominate); "deepest bounded" pins
    // the node just above the first opaque/unfoldable operand.  A `Load[reg]`
    // function pointer has no bounded dominator → `None` → deferred, no fold.
    let (idx_value, range) = decompose_index(function, ranges, target_value, branch)?;

    // Pin the index over its proven strided range and let the read-only
    // evaluator fold the index-pruned dispatch cone for every value: rodata
    // reads via `LoadReadOnly`, on-stack reads via `reaching_store`, arithmetic
    // via ConstFold.  Every value must fold to a constant target (fail-closed):
    // a dispatch whose base is symbolic — e.g. a vtable `Load[reg + idx*8]` —
    // does not fold and the branch defers.  No size guard on the cone is needed:
    // the evaluator identifies the SP spine structurally (no per-node cone walk),
    // so even a false-positive candidate with a large decode cone folds cheaply
    // and the fold below bails on its first non-folding value.
    let mut ev = super::eval::Evaluator::new(function, rom, alias_mode);
    let pruned = super::eval::cone_order_pruned(function, target_value, idx_value);
    // Enumerate the table by folding the dispatch for every value in the strided
    // range `{lo, lo+stride, … hi}`.  `stride` is a KnownBits MUST-divisor of the
    // value spacing, so stepping by it visits exactly the reachable indices (a
    // scaled `idx*8` hits 8,16,… not the 7 misaligned values between);
    // `Interval::count` already capped the total.  `collect::<Option<_>>` bails to
    // `None` the moment a value fails to fold (this candidate is not the index,
    // or the table is not fully resolvable → fail closed).
    let step = usize::try_from(range.stride).unwrap_or(1).max(1);
    let mut targets: Vec<u64> = (range.lo..=range.hi)
        .step_by(step)
        .map(|x| ev.eval_target(&pruned, target_value, idx_value, x))
        .collect::<Option<_>>()?;
    targets.sort_unstable();
    targets.dedup();
    (!targets.is_empty()).then_some(ResolvedTargets::Multiple(targets))
}

/// THE dispatch index: the **deepest** genuinely-bounded (guard- or
/// mask-constrained strided range, never width-only) non-constant value that
/// **dominates the target** in its variability cone.
///
/// A jump table is `branch f(index)` for one controlling variable, so the index
/// is a node **every** variable→target path flows through — a *value-dominator*
/// of the target.  Restricting to dominators excludes a bypassed sub-branch: in
/// a rotate `index = (x<<2) | (x>>30)`, `x>>30` has the tightest interval but
/// does **not** dominate the target (the `x<<2` arm bypasses it); `x` and the
/// `Or` *do*.  Among the dominators we take the **deepest** bounded one — the
/// node sitting just above the first opaque/unfoldable operand.  That is the
/// most-refined index: a scaled `idx*8` and its source `idx` both dominate, and
/// `idx` (deeper) is the one to enumerate; a `Load[base+(SegmentOp(idx)&7)*8]`
/// stops at `SegmentOp&7` because `SegmentOp` is an opaque cone leaf its input
/// `idx` never joins.  The count-capped strided range (`bounded_index` via
/// [`Interval::count`]) is what makes a wide-but-sparse `idx*8` (601 entries
/// over a 4800 span) enumerable while a dense 4800-wide impostor is not.
///
/// The cone is built by walking `target`'s variability inputs, TRAVERSING THROUGH
/// a load the evaluator can fold (const-base rodata / SP-rooted stack) into its
/// address and stopping at a reg/GOT-based load (vtable / funcptr / PIC) and at
/// opaque sources.  A virtual ENTRY feeds every cone root, and
/// [`petgraph::algo::dominators::simple_fast`] yields the target's dominator
/// chain — the short "convergence → target" spine, not the whole cone.  The pin
/// is still confirmed by the caller's fold (belt-and-braces).
fn decompose_index(
    function: &strider_ir::Function,
    ranges: &mut crate::value_range::RangeMap<'_>,
    target: ValueId,
    branch: NodeId,
) -> Option<(ValueId, Interval)> {
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
        if function.int_const_u128(v).is_some() || !seen.insert(v) {
            continue;
        }
        let vi = node_of(&mut g, &mut nidx, v);
        // `v`'s variability inputs: the addressing arithmetic, a foldable load's
        // address, or nothing for an opaque source.
        let inputs: Vec<ValueId> = match function.node_kind(function.producer(v)) {
            NodeKind::Load(_) => foldable_load_address(function, v).into_iter().collect(),
            NodeKind::InitialVar(_)
            | NodeKind::Phi
            | NodeKind::Call
            | NodeKind::CallOther { .. }
            | NodeKind::New
            | NodeKind::SegmentOp { .. }
            | NodeKind::CPoolRef => Vec::new(),
            _ => function.int_inputs(v).collect(),
        };
        let mut has_var_input = false;
        for p in inputs {
            // A const or an SP-decomposable base (`sp`, `sp+K`, alignment-masked
            // `sp & -16`) is a symbolic *base*, not a variable: the evaluator
            // keeps it as `SpRel` and never enumerates it.  Skipping it (like a
            // const) keeps `sp` from being a second root — otherwise the real
            // index fails to dominate the target (the SP path bypasses it) and
            // every stack table would defer.  `decompose_readonly` is the single
            // SSoT for "is this a pure SP base": it recognises exactly the
            // `sp + const` / alignment-masked shapes and (unlike a structural
            // `sp & mask` check) rejects a bit-extraction `sp & 0xF`, which is a
            // bounded *value* the walk must keep as a candidate index.
            if function.int_const_u128(p).is_some()
                || crate::sp_expr::decompose_readonly(function, p).is_some()
            {
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

    let target_idx = *nidx.get(&target)?;
    let doms = petgraph::algo::dominators::simple_fast(&g, entry);

    // `dominators` yields the chain shallow→deep (target, idom, … root).  Collect
    // the value ids (cheap), then walk them root-ward and return the FIRST
    // genuinely-bounded one — the DEEPEST index, just above the first opaque
    // operand the cone couldn't traverse.  Reversing (rather than a forward
    // `.last()`) early-exits at that deepest hit, so the heavy `bounded_index`
    // range query runs on as few nodes as possible.
    let mut load_memo = rustc_hash::FxHashMap::default();
    let chain: Vec<ValueId> = doms
        .dominators(target_idx)?
        .filter_map(|di| *g.node_weight(di).expect("dominator is a graph node"))
        .filter(|&v| v != target)
        .collect();
    chain
        .into_iter()
        .rev()
        .find_map(|v| bounded_index(function, ranges, branch, v, &mut load_memo))
}

/// The address of a load the abstract evaluator can fold -- one whose address is
/// (or has an operand that is) a const (rodata, resolved by `LoadReadOnly`) or an
/// SP-rooted address (stack, resolved by `reaching_store`) -- or `None` for a
/// reg / GOT-based load (vtable / funcptr / PIC) it cannot.  Traversing into a
/// foldable load's address continues the index search past a table's entry load;
/// stopping at an unfoldable one lets the branch defer without chasing a
/// non-existent index.
///
/// A stack *table* load `Load[(sp+base) + idx*stride]` carries an
/// **index-dependent** address that never `decompose`s to a fixed `(base,
/// offset)`, so we can't ask about the address as a whole — but its `sp+base`
/// operand *does* decompose.  Hence the operand-level check: the address is
/// foldable when it, OR any of its operands, is a base.
fn foldable_load_address(function: &strider_ir::Function, load: ValueId) -> Option<ValueId> {
    let addr = function.int_inputs(load).next()?;
    let foldable =
        is_base_operand(function, addr) || function.int_inputs(addr).any(|op| is_base_operand(function, op));
    foldable.then_some(addr)
}

/// A const address (rodata table base) or an SP-rooted address (stack table
/// base) -- the two bases the evaluator can fold a `Load` through.  SP-rooting
/// is asked of the shared `decompose_readonly` (the single SSoT) — no bespoke
/// structural SP walk — and the operand check in [`foldable_load_address`]
/// bridges the index-dependent case that decompose returns `None` for.
fn is_base_operand(function: &strider_ir::Function, v: ValueId) -> bool {
    function.int_const_u128(v).is_some() || crate::sp_expr::decompose_readonly(function, v).is_some()
}

/// `v` as a candidate index: a genuinely-bounded (guard-/mask-constrained, never
/// width-only) non-constant integer.  Excludes a loaded table *entry* -- a
/// load-derived value bounded only by its load width (`ZeroExtend(Load.byte)`
/// spanning `[0,255]`) with no dominating guard and no explicit mask --
/// enumerating one folds to bogus sequential targets.
fn bounded_index(
    function: &strider_ir::Function,
    ranges: &mut crate::value_range::RangeMap<'_>,
    branch: NodeId,
    v: ValueId,
    load_memo: &mut rustc_hash::FxHashMap<ValueId, bool>,
) -> Option<(ValueId, Interval)> {
    let ty = function
        .value_type_opt(v)
        .filter(|t| t.is_integer() && function.int_const_u128(v).is_none())?;
    let iv = ranges.range_of(v, branch);
    // Cap on the ENTRY COUNT the classifier enumerates, not the raw span: a
    // strided `idx*8 = [0, 4800, stride 8]` is 601 entries (enumerable), while a
    // dense 4800-wide range is not.  This is what separates "wide but finite"
    // from "opaque".
    let bounded =
        iv.hi >= iv.lo && iv.hi < ty.bit_mask_u128() && iv.count() <= u128::from(MAX_TABLE_ENTRIES);
    let entry_load = is_load_derived(function, v, load_memo)
        && ranges.dominating_guard(v, branch).is_none()
        && !is_and_masked(function, v);
    (bounded && !entry_load).then_some((v, iv))
}


/// Is `v` (transitively) the output of a `Load`?  A loaded value is a table
/// *entry*, not the table *index*, so it must not be enumerated as one.
fn is_load_derived(
    function: &strider_ir::Function,
    v: ValueId,
    memo: &mut rustc_hash::FxHashMap<ValueId, bool>,
) -> bool {
    if let Some(&cached) = memo.get(&v) {
        return cached;
    }
    // Pre-seed `false` to break cycles (a value Phi can be self-referential).
    memo.insert(v, false);
    let node = function.producer(v);
    let result = matches!(function.node_kind(node), NodeKind::Load(_))
        || function
            .node_inputs(node)
            .into_iter()
            .any(|input| function.value_type_opt(input).is_some() && is_load_derived(function, input, memo));
    memo.insert(v, result);
    result
}

/// Is `v` produced by an `And(_, IntConst)`?  Such a value is mask-bounded
/// (e.g. `kind & 7`), a legitimate dispatch index even when the masked operand
/// was loaded — unlike a table entry, whose bound is only its load width.
fn is_and_masked(function: &strider_ir::Function, v: ValueId) -> bool {
    let node = function.producer(v);
    matches!(function.node_kind(node), NodeKind::IntBinaryOp(IntBinaryOp::And))
        && function
            .node_inputs(node)
            .into_iter()
            .any(|input| function.int_const_u128(input).is_some())
}

#[cfg(test)]
#[path = "table_tests.rs"]
mod table_tests;
