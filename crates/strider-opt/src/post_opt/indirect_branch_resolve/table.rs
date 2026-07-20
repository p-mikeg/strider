//! Table-dispatch arm of the indirect-branch classifier.
//!
//! A switch lowers to `branch f(index)` for one bounded index, whether the
//! table lives in rodata, on the stack, or behind a GP/GOT indirection.  Rather
//! than pattern-match each addressing shape, this arm finds the index and then
//! delegates the addressing to the abstract evaluator, pinning the index across
//! its proven strided range and folding the dispatch cone for each value.
//!
//! The evaluator covers only ConstFold arithmetic, `LoadReadOnly` ROM reads,
//! and `LoadForward` via `reaching_store`.  A cone needing anything else
//! resolves to `None` and the branch defers: sound, just less eager.
//!
//! # Soundness
//!
//! Two independent gates must hold before committing to `Multiple`.
//!
//! A BOUNDED INDEX, from an `if (idx < N)` guard, a mask, or a shift.  The cap
//! is on the entry COUNT, not the span, so a sparse scaled `idx*8` is
//! enumerable while a dense wide range is not.  A width-only range, one exactly
//! filling its extend-stripped type width, is a raw loaded table ENTRY and is
//! rejected, so table data is never enumerated as an index.
//!
//! A COMPLETE FOLD: every value in the range must fold to a constant target.
//! Any failure returns `None`, because a `Multiple` omitting a real runtime
//! target would wire a CFG with missing edges.
//!
//! Over-approximating the bound is sound, since surplus targets become dead CFG
//! edges.  Under-approximating is not.

#![allow(clippy::module_name_repetitions)]

use super::MAX_TABLE_ENTRIES;
use crate::value_range::Interval;
use crate::{AliasMode, ReadOnlyMemory};
use petgraph::graph::{DiGraph, NodeIndex};
use strider_cfg::ResolvedTargets;
use strider_ir::IRViewer;
use strider_ir::node::{ExtendOp, NodeId, NodeKind, ValueId};

/// `rom` is the binary's read-only image; `None` disables the rodata arm.  The
/// stack-pointer varnode and the target endianness come off `function`.
#[must_use]
pub fn classify_table_dispatch(
    function: &strider_ir::Function,
    branch: NodeId,
    rom: Option<&dyn ReadOnlyMemory>,
    ranges: &mut crate::value_range::RangeMap<'_>,
    alias_mode: AliasMode,
) -> Option<ResolvedTargets> {
    // Taking the branch NODE rather than the bare value scopes the index-range
    // query below to the branch ACTUALLY being resolved, never the first
    // `IndirectBranch` that happens to share the dispatch value.
    let target_value = function.indirect_branch_target(branch);

    // A `Load[reg]` function pointer has no bounded dominator and defers here.
    let (idx_value, range) = decompose_index(function, ranges, target_value, branch)?;

    let mut ev = super::eval::Evaluator::new(function, rom, alias_mode);
    let pruned = super::eval::cone_order_pruned(function, target_value, idx_value);
    // `stride` is a KnownBits MUST-divisor of the value spacing, so stepping by
    // it visits exactly the reachable indices.  `collect::<Option<_>>` bails the
    // moment a value fails to fold, which fails closed.
    let step = usize::try_from(range.stride).unwrap_or(1).max(1);
    let mut targets: Vec<u64> = (range.lo..=range.hi)
        .step_by(step)
        .map(|x| ev.eval_target(&pruned, target_value, idx_value, x))
        .collect::<Option<_>>()?;
    targets.sort_unstable();
    targets.dedup();
    (!targets.is_empty()).then_some(ResolvedTargets::Multiple(targets))
}

/// The shallowest genuinely-bounded, non-width-only, non-constant value that
/// DOMINATES the target in its variability cone.
///
/// Requiring dominance excludes a bypassed sub-branch: in a rotate
/// `(x<<2) | (x>>30)`, `x>>30` has the tightest interval but does not dominate.
///
/// SHALLOWEST here is target-rooted (nearest the dispatch value), the opposite
/// of the entry-rooted "deepest-dominator" wording elsewhere. It, not the
/// deepest, is load-bearing: it sits just below the address arithmetic with
/// every guard/mask/shift applied, so enumerating it visits exactly the
/// reachable slots.  A deeper (further-from-dispatch) node's bound can be
/// looser, and enumerating out-of-bounds slots defers a branch that should have
/// resolved.
///
/// The cone traverses THROUGH a load the evaluator can fold (const-base rodata,
/// SP-rooted stack) into its address, and stops at reg/GOT-based loads (vtable,
/// funcptr, PIC) and opaque sources.  A virtual ENTRY feeds every root.
fn decompose_index(
    function: &strider_ir::Function,
    ranges: &mut crate::value_range::RangeMap<'_>,
    target: ValueId,
    branch: NodeId,
) -> Option<(ValueId, Interval)> {
    // Node weight `None` marks the virtual ENTRY, `Some(v)` a cone value.
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
        // The addressing arithmetic, a foldable load's address, or nothing for
        // an opaque source.
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
            // An SP-decomposable base is a symbolic BASE, not a variable, so it
            // is skipped like a const; otherwise `sp` becomes a second root and
            // the real index stops dominating the target.  `decompose` accepts
            // `sp + const` and alignment masks but rejects `sp & 0xF`, which is
            // a bounded VALUE and must stay a candidate index.
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
            g.add_edge(entry, vi, ()); // a cone root
        }
    }

    let target_idx = *nidx.get(&target)?;
    let doms = petgraph::algo::dominators::simple_fast(&g, entry);

    // `dominators` yields the chain shallow to deep, so the first bounded hit
    // IS the shallowest.
    doms.dominators(target_idx)?
        .filter_map(|di| *g.node_weight(di).expect("dominator is a graph node"))
        .filter(|&v| v != target)
        .find_map(|v| bounded_index(function, ranges, branch, v))
}

/// The address of a load the evaluator can fold, or `None` for a reg/GOT-based
/// one (vtable, funcptr, PIC) it cannot.
///
/// The check is operand-level because a stack table load
/// `Load[(sp+base) + idx*stride]` has an INDEX-DEPENDENT address that never
/// decomposes as a whole, though its `sp+base` operand does.
fn foldable_load_address(function: &strider_ir::Function, load: ValueId) -> Option<ValueId> {
    let addr = function.int_inputs(load).next()?;
    let foldable = is_base_operand(function, addr)
        || function
            .int_inputs(addr)
            .any(|op| is_base_operand(function, op));
    foldable.then_some(addr)
}

/// The two bases the evaluator can fold a `Load` through: a const rodata base
/// or an SP-rooted stack base.
fn is_base_operand(function: &strider_ir::Function, v: ValueId) -> bool {
    function.int_const_u128(v).is_some() || crate::sp_analysis::decompose(function, v).is_some()
}

/// A genuinely-bounded non-constant integer whose bound is a real narrowing,
/// not just its type width.  A loaded table ENTRY, bounded only by its load
/// width, is excluded: enumerating one folds to bogus targets.
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

/// Is `v`'s range merely its type width rather than a real narrowing?  A raw
/// byte load fills its cell width exactly, making it table DATA, not an index.
///
/// Keying on the RANGE rather than on "is it load-derived" is load-bearing: a
/// guarded raw load, `if (Load < N) switch(Load)`, is a genuine index even
/// though it strips to a `Load`.
///
/// Zero-extends are stripped first because they preserve the integer value
/// while widening the type.  `w < 128` keeps the shift well-defined.
fn is_width_only(function: &strider_ir::Function, v: ValueId, iv: Interval) -> bool {
    let mut base = v;
    while matches!(
        function.node_kind(function.producer(base)),
        NodeKind::Extend(ExtendOp::ZeroExtend)
    ) {
        match function.int_inputs(base).next() {
            Some(inner) => base = inner,
            None => break,
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
