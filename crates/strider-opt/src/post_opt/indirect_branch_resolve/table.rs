//! Table-dispatch arm of the indirect-branch classifier.
//!
//! A switch lowers to `branch f(index)` for one bounded `index` — whether the
//! table lives in rodata (`Load[ base + idx*stride ]`, absolute base), on the
//! stack (`Load[ (sp + K) + idx*stride ]`, SP-rooted), or behind a GP/GOT
//! indirection.  Rather than pattern-match each addressing shape, this arm
//! **delegates the addressing to the abstract evaluator**:
//!
//! 1. **Find THE index** (`decompose_index`).  A jump table is `f(index)` for
//!    one controlling variable, so the index is a value every variable→target
//!    path flows through — a *value-dominator* of the target.  Build the
//!    dispatch's variability cone (walking the target's integer ancestors,
//!    traversing *into* a foldable load's address and stopping at reg/GOT loads
//!    and opaque sources), take the target's dominator chain, and pick the
//!    SHALLOWEST genuinely-bounded value on it — the fully-narrowed index just
//!    below the (width-only/unbounded) address arithmetic.  Dominance excludes a
//!    bypassed sub-branch (a rotate's `x>>30`); "shallowest" picks the index
//!    with every guard/mask/shift applied, not a looser pre-narrowing ancestor.
//!    A `Load[reg]` function pointer has no bounded dominator → deferred, no fold.
//! 2. **Pin and fold.**  Evaluate the dispatch cone under `index = i` via the
//!    read-only `super::eval::Evaluator` (ConstFold arithmetic + `LoadReadOnly`
//!    ROM reads + `LoadForward` via `reaching_store`) for every `i` in the
//!    index's proven **strided** range.  The dispatch value is a concrete
//!    constant iff the addressing fully resolved; the folded constants are the
//!    targets.  The evaluator covers *only* those three foldings, so a cone that
//!    would need some other pass (e.g. a `KnownBits` narrowing) resolves to
//!    `None` and the branch defers — sound, just less eager.
//!
//! ## Soundness
//!
//! Two independent gates must hold to commit to `Multiple`:
//!
//! 1. **Bounded index.**  The range analysis bounds `idx` from an `if (idx < N)`
//!    guard, a mask (`idx & 0x7`), or a shift (`b >> 5`); the cap is on the
//!    *entry count* `(hi−lo)/stride + 1` (see [`Interval::count`]), so a sparse
//!    scaled `idx*8` is enumerable while a dense wide range is not.  A *width-only*
//!    range — one that exactly fills its (extend-stripped) type width, i.e. a raw
//!    loaded table *entry* like a `[0,255]` byte — is rejected (`is_width_only`),
//!    so table *data* is never enumerated as an index.
//!
//! 2. **Complete fold.**  *Every* value in the strided range must fold to a
//!    constant target; any failure returns `None` (a `Multiple` omitting a real
//!    runtime target would wire a CFG with missing edges).  The evaluator is
//!    read-only, so the analysed function is never mutated, and the caller's
//!    [`AliasMode`] is threaded in so a global-clobbered on-stack table defers
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
use petgraph::graph::{DiGraph, NodeIndex};
use strider_cfg::ResolvedTargets;
use strider_ir::{IRViewer, IntBinaryOp};
use strider_ir::node::{ExtendOp, NodeId, NodeKind, ValueId};

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

    // Find THE index: the shallowest value on the dispatch's variability cone
    // that both dominates the target and carries a bounded strided range
    // (`decompose_index`).  Dominance rules out structural impostors (a rotate's
    // `x>>30` has a tight range but doesn't dominate); "shallowest bounded" pins
    // the fully-narrowed index just below the address arithmetic.  A `Load[reg]`
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

/// THE dispatch index: the **shallowest** genuinely-bounded, non-width-only (see
/// [`is_width_only`]) non-constant value that **dominates the target** in its
/// variability cone.
///
/// A jump table is `branch f(index)` for one controlling variable, so the index
/// is a node **every** variable→target path flows through — a *value-dominator*
/// of the target.  Restricting to dominators excludes a bypassed sub-branch: in
/// a rotate `index = (x<<2) | (x>>30)`, `x>>30` has the tightest interval but
/// does **not** dominate the target (the `x<<2` arm bypasses it); `x` and the
/// `Or` *do*.  Among the dominators we take the **shallowest** bounded one — the
/// value sitting just below the address arithmetic (`zext`, `*stride`,
/// `base + …`, all width-only/unbounded and skipped).  That is the *fully
/// narrowed* index, with every guard/mask/shift already applied: enumerating it
/// visits exactly the reachable table slots.  A **deeper** bounded node is an
/// *earlier* stage of the same computation whose bound can be looser — e.g. a
/// pre-guard `b>>5` spans `[0,7]` while the guarded value that actually indexes
/// the table is `[0,5]`; enumerating the looser `[0,7]` reads two out-of-bounds
/// slots and the fold fails, so the branch would wrongly defer.  The count-capped
/// strided range (`bounded_index` via [`Interval::count`]) is what makes a
/// wide-but-sparse `idx*8` (601 entries over a 4800 span) enumerable while a
/// dense 4800-wide impostor is not.
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
    // Build the value-dominance graph of the cone: a virtual ENTRY with an edge
    // to every root, and a producer→consumer edge for every variability edge
    // (traversing through a foldable load into its address).  Node weight
    // `None` marks the ENTRY; `Some(v)` a cone value.
    let mut g: DiGraph<Option<ValueId>, ()> = DiGraph::new();
    let entry = g.add_node(None);
    let mut nidx: rustc_hash::FxHashMap<ValueId, NodeIndex> = rustc_hash::FxHashMap::default();
    let node_of =
        |g: &mut DiGraph<Option<ValueId>, ()>,
         nidx: &mut rustc_hash::FxHashMap<ValueId, NodeIndex>,
         v: ValueId| { *nidx.entry(v).or_insert_with(|| g.add_node(Some(v))) };

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
            // every stack table would defer.  `decompose` is the single
            // SSoT for "is this a pure SP base": it recognises exactly the
            // `sp + const` / alignment-masked shapes and (unlike a structural
            // `sp & mask` check) rejects a bit-extraction `sp & 0xF`, which is a
            // bounded *value* the walk must keep as a candidate index.
            if function.int_const_u128(p).is_some()
                || crate::sp_analysis::decompose(function, p).is_some()
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

    // `dominators` yields the chain shallow→deep (target, idom, … root).  Walk it
    // in that order (target-ward first) and return the FIRST genuinely-bounded
    // one — the SHALLOWEST index, sitting just below the width-only address
    // arithmetic.  Taking the shallowest (not the deepest) picks the fully
    // narrowed value: a deeper ancestor is an earlier stage whose bound can be
    // looser (a pre-guard `b>>5` over `[0,7]` vs the guarded index `[0,5]`).
    // `find_map` early-exits at the first hit, so the heavy `bounded_index` range
    // query runs on as few nodes as possible.
    doms.dominators(target_idx)?
        .filter_map(|di| *g.node_weight(di).expect("dominator is a graph node"))
        .filter(|&v| v != target)
        .find_map(|v| bounded_index(function, ranges, branch, v))
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
    let foldable = is_base_operand(function, addr)
        || function
            .int_inputs(addr)
            .any(|op| is_base_operand(function, op));
    foldable.then_some(addr)
}

/// A const address (rodata table base) or an SP-rooted address (stack table
/// base) -- the two bases the evaluator can fold a `Load` through.  SP-rooting
/// is asked of the shared `decompose` (the single SSoT) — no bespoke
/// structural SP walk — and the operand check in [`foldable_load_address`]
/// bridges the index-dependent case that decompose returns `None` for.
fn is_base_operand(function: &strider_ir::Function, v: ValueId) -> bool {
    function.int_const_u128(v).is_some() || crate::sp_analysis::decompose(function, v).is_some()
}

/// `v` as a candidate index: a genuinely-bounded non-constant integer whose
/// bound is a real narrowing, not just its type width.  Excludes a loaded table
/// *entry* -- a value bounded only by its load width (`ZeroExtend(Load.byte)`
/// spanning `[0,255]`) -- since enumerating one folds to bogus targets.
fn bounded_index(
    function: &strider_ir::Function,
    ranges: &mut crate::value_range::RangeMap<'_>,
    branch: NodeId,
    v: ValueId,
) -> Option<(ValueId, Interval)> {
    let ty = function
        .value_type_opt(v)
        .filter(|t| t.is_integer() && function.int_const_u128(v).is_none())?;
    let iv = ranges.range_of(v, branch);
    let bounded =
        iv.hi >= iv.lo && iv.hi < ty.bit_mask_u128() && iv.count() <= u128::from(MAX_TABLE_ENTRIES);
    (bounded && !is_width_only(function, v, iv)).then_some((v, iv))
}

/// Is `v`'s range merely its *type width* rather than a real narrowing?  A raw
/// byte load (or one zero-extended for addressing) has a range that exactly
/// fills its cell width -- it is table *data*, not an *index*, and enumerating
/// its `[0,255]` folds to bogus targets.  A value narrowed by a shift (`b>>5`),
/// mask (`b&7`), or guard (`b<N`) instead has a range strictly *inside* its
/// (possibly wider) integer type, so it is a real index.
///
/// Keying on the *range* (not on whether the value is load-derived) is load
/// bearing: a **guarded raw load** — `if (Load < N) switch(Load)` — is a genuine
/// index whose `[0,N-1]` bound comes from the guard, so it must be accepted even
/// though it strips to a `Load`.  A pure "skip all loads" rule cannot tell that
/// guard-narrowed load from a raw `[0,255]` entry; the width comparison can.
///
/// A `ZeroExtend` preserves the integer value, so the range is unchanged across
/// it while the type widens (a byte's `[0,255]` reads as full-width against `i8`
/// but narrow against `i32`).  We strip zero-extends to the originating node and
/// compare the range against *that* node's own width.  (`bounded` already caps
/// the count, so in practice only a full byte reaches the width test; the
/// `w < 128` guard keeps the shift well-defined for wide types.)
fn is_width_only(function: &strider_ir::Function, v: ValueId, iv: Interval) -> bool {
    let mut base = v;
    loop {
        match function.node_kind(function.producer(base)) {
            // A `ZeroExtend` preserves the integer value (range unchanged, type
            // widens) — strip to the originating node and test its own width.
            NodeKind::Extend(ExtendOp::ZeroExtend) => match function.int_inputs(base).next() {
                Some(inner) => base = inner,
                None => break,
            },
            // A CONSTANT left-shift SCALES a value (`table_byte << 1` for a
            // halfword-offset TBB/TBH table) — a bijection into the low bits, so
            // it preserves the value COUNT (`{0,2,…,2·255}` still has 256 values,
            // just stride 2).  The lifter canonicalises `mul(x, 2^k)` to this
            // shape, so a table-DATA byte scaled by the entry size now reaches
            // here as `ShiftLeft(zext(load), k)`; peel it so its `[0,(2^w-1)·2^k]`
            // range is recognised as the byte's own width, not a real index.
            NodeKind::IntBinaryOp(IntBinaryOp::ShiftLeft) => {
                let mut it = function.int_inputs(base);
                match (it.next(), it.next()) {
                    (Some(lhs), Some(rhs)) if function.int_const_u128(rhs).is_some() => base = lhs,
                    _ => break,
                }
            }
            _ => break,
        }
    }
    function
        .value_type_opt(base)
        .map(|t| t.bit_width())
        .is_some_and(|w| w < 128 && iv.count() == 1u128 << w)
}

#[cfg(test)]
#[path = "table_tests.rs"]
mod table_tests;
