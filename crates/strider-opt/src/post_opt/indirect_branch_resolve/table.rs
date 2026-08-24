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
//! resolves to `None` and the branch defers: sound but less eager.
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
use strider_cfg::{ResolvedTarget, ResolvedTargets};
use strider_ir::IRViewer;
use strider_ir::IntBinaryOp;
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
    mode_value: Option<ValueId>,
) -> Option<ResolvedTargets> {
    let target_value = function.indirect_branch_target(branch);
    classify_dispatch_value(
        function,
        branch,
        target_value,
        rom,
        ranges,
        alias_mode,
        mode_value,
    )
}

/// `classify_table_dispatch` against an explicit dispatch value, so an
/// already-seated `Switch` can be re-derived from its selector.
///
/// `site` is the `IndirectBranch` or `Switch` being resolved; it scopes the
/// index-range query to that node, never the first one that happens to share
/// the dispatch value.
pub fn classify_dispatch_value(
    function: &strider_ir::Function,
    site: NodeId,
    target_value: ValueId,
    rom: Option<&dyn ReadOnlyMemory>,
    ranges: &mut crate::value_range::RangeMap<'_>,
    alias_mode: AliasMode,
    mode_value: Option<ValueId>,
) -> Option<ResolvedTargets> {
    // A `Load[reg]` function pointer has no bounded dominator and defers here.
    let (idx_value, range) = decompose_index(function, ranges, target_value, site)?;

    let mut ev = super::eval::Evaluator::new(function, rom, alias_mode);
    let pruned = super::eval::cone_order_pruned(function, target_value, idx_value);
    // The branch's committed ISA mode (an interworking `bx`/`jr`-dispatch),
    // `(entry & 1)`, evaluated per index so each arm carries its own mode.
    let mode_pruned = mode_value.map(|mv| super::eval::cone_order_pruned(function, mv, idx_value));
    // `stride` is a KnownBits MUST-divisor of the value spacing, so stepping by
    // it visits exactly the reachable indices.  `collect::<Option<_>>` bails the
    // moment a value fails to fold, which fails closed.  A stride wider than
    // `usize` (a scaled wide-type range) defers rather than silently stepping by
    // 1 and enumerating the full dense span.
    let step = usize::try_from(range.stride).ok()?.max(1);
    let mut targets: Vec<ResolvedTarget> = (range.lo..=range.hi)
        .step_by(step)
        .map(|x| {
            // One memo for both roots: they are siblings over one dispatch
            // word (`And(word, 1)` and `And(word, ~1)`), so everything under
            // `word` folds once.
            ev.begin_index(idx_value, x);
            let addr = ev.eval_root(&pruned, target_value)?;
            let isa_bit = match (mode_value, &mode_pruned) {
                // The branch commits an ISA mode per target; that mode MUST fold.
                // If it does not, fail closed (`?` defers the whole branch),
                // rather than decode the target in a guessed mode.
                (Some(mv), Some(order)) => Some(ev.eval_root(order, mv)? != 0),
                // No mode switch bound to this branch: an in-mode jump table
                // that inherits the mode flowing into the branch.
                _ => None,
            };
            Some(ResolvedTarget::new(addr, isa_bit))
        })
        .collect::<Option<_>>()?;
    targets.sort_by_key(|t| t.addr);
    // `addr` is mode-bit-masked, so words `X` and `X | 1` land on one address
    // carrying opposite modes.  A disagreement defers the whole site: `None`
    // is the mode FLOWING into the branch, which a mode-committing branch
    // contradicts.
    let mut deduped: Vec<ResolvedTarget> = Vec::with_capacity(targets.len());
    for target in targets {
        match deduped.last() {
            Some(kept) if kept.addr == target.addr => {
                if kept.isa_bit != target.isa_bit {
                    return None;
                }
            }
            _ => deduped.push(target),
        }
    }
    (!deduped.is_empty()).then_some(ResolvedTargets::Multiple(deduped))
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
    site: NodeId,
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
                || crate::mem_analysis::decompose(function, p).is_some()
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
        .find_map(|v| bounded_index(function, ranges, site, v))
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
    function.int_const_u128(v).is_some() || crate::mem_analysis::decompose(function, v).is_some()
}

/// A genuinely-bounded non-constant integer whose bound is a real narrowing,
/// not just its type width.  A loaded table ENTRY, bounded only by its load
/// width, is excluded: enumerating one folds to bogus targets.
fn bounded_index(
    function: &strider_ir::Function,
    ranges: &mut crate::value_range::RangeMap<'_>,
    site: NodeId,
    v: ValueId,
) -> Option<(ValueId, Interval)> {
    let ty = function
        .value_type_opt(v)
        .filter(|t| t.is_integer() && function.int_const_u128(v).is_none())?;
    let iv = ranges.range_of(v, site);
    (index_bound_ok(ty, iv) && !is_width_only(function, v, iv)).then_some((v, iv))
}

/// Is `iv` a non-empty, enumerable, genuinely-narrowed range at `ty`?
///
/// Past 128 bits the interval's `u128` carrier cannot represent the type's own
/// top, so every interval there reads as narrowed.
fn index_bound_ok(ty: strider_ir::node::ValueType, iv: Interval) -> bool {
    crate::opt::known_bits::type_mask_u128(ty).is_some_and(|mask| {
        iv.hi >= iv.lo && iv.hi < mask && iv.count() <= u128::from(MAX_TABLE_ENTRIES)
    })
}

/// The variable operand and the constant of a non-commutative binop whose RHS
/// is constant.
fn strip_const_scale(function: &strider_ir::Function, v: ValueId) -> Option<(ValueId, u128)> {
    let [lhs, rhs] = function.producer_inputs_exact::<2>(v).ok()?;
    function.int_const_u128(rhs).map(|c| (lhs, c))
}

/// As [`strip_const_scale`], for a commutative binop: the constant may sit on
/// either side, and two constants are not a scaling of anything.
fn strip_commutative_const_scale(
    function: &strider_ir::Function,
    v: ValueId,
) -> Option<(ValueId, u128)> {
    let [lhs, rhs] = function.producer_inputs_exact::<2>(v).ok()?;
    match (function.int_const_u128(lhs), function.int_const_u128(rhs)) {
        (None, Some(c)) => Some((lhs, c)),
        (Some(c), None) => Some((rhs, c)),
        _ => None,
    }
}

/// One constant scaling on the way from a cell to `v`, collected outermost
/// first.
enum Scale {
    /// `* m`: spreads the value set out, so a later divide collapses less.
    Widen(u128),
    /// `/ c`: collapses the value set once it outruns the spacing.
    Narrow(u128),
}

/// Is `v`'s range merely its type width rather than a real narrowing?  A raw
/// byte load fills its cell width exactly, making it table DATA, not an index.
///
/// Keying on the RANGE rather than on "is it load-derived" is load-bearing: a
/// guarded raw load, `if (Load < N) switch(Load)`, is a genuine index even
/// though it strips to a `Load`.
///
/// Zero-extends and constant scalings are stripped first, then replayed
/// innermost-first over the cell's `1 << w` consecutive values, tracking their
/// spacing: a divide collapses the set only by however much it outruns the
/// spacing a preceding multiply built up, so `(cell << 2) >> 1` keeps all
/// `1 << w`.  The count is then capped by what the OUTPUT width can hold at
/// that spacing, since a widening scale wraps: `zext(i8) << 25` at `I32` has
/// 128 distinct values, not 256.  A scale past the `u128` carrier, and a floor
/// over an unrelated spacing, both answer "width-only", which defers the site
/// rather than enumerating a cell.  An unrecognised producer is not a failure:
/// the strip ends there and the count is taken at that value's own width.
/// `w < 128` keeps the shift well-defined.
fn is_width_only(function: &strider_ir::Function, v: ValueId, iv: Interval) -> bool {
    let mut base = v;
    let mut chain: Vec<Scale> = Vec::new();
    loop {
        let producer = function.producer(base);
        let inner = match *function.node_kind(producer) {
            // Preserves the integer value while widening the type.
            NodeKind::Extend(ExtendOp::ZeroExtend) => {
                function.int_inputs(base).next().map(|next| (next, None))
            }
            NodeKind::IntBinaryOp(IntBinaryOp::ShiftLeft) => strip_const_scale(function, base)
                .and_then(|(next, k)| Some((next, Some(Scale::Widen(pow2(k)?))))),
            NodeKind::IntBinaryOp(IntBinaryOp::ShiftRight) => strip_const_scale(function, base)
                .and_then(|(next, k)| Some((next, Some(Scale::Narrow(pow2(k)?))))),
            NodeKind::IntBinaryOp(IntBinaryOp::Div) => strip_const_scale(function, base)
                .and_then(|(next, c)| (c != 0).then_some((next, Some(Scale::Narrow(c))))),
            NodeKind::IntBinaryOp(IntBinaryOp::Mul) => {
                strip_commutative_const_scale(function, base)
                    .and_then(|(next, c)| (c != 0).then_some((next, Some(Scale::Widen(c)))))
            }
            _ => None,
        };
        match inner {
            Some((next, scale)) => {
                base = next;
                chain.extend(scale);
            }
            None => break,
        }
    }
    let Some(w) = function.value_type_opt(base).map(|t| t.bit_width()) else {
        return false;
    };
    if w >= 128 {
        return false;
    }
    let mut count: u128 = 1u128 << w;
    let mut spacing: u128 = 1;
    for scale in chain.iter().rev() {
        match *scale {
            Scale::Widen(m) => match spacing.checked_mul(m) {
                Some(s) => spacing = s,
                None => return true,
            },
            Scale::Narrow(c) if spacing.is_multiple_of(c) => spacing /= c,
            Scale::Narrow(c) if c.is_multiple_of(spacing) => {
                count = count.div_ceil(c / spacing);
                spacing = 1;
            }
            // Floor over a spacing that neither divides nor is divided leaves
            // no arithmetic progression to count.
            Scale::Narrow(_) => return true,
        }
    }
    // A widening scale is modular at the output width: values `spacing` apart
    // repeat after `2^out_w / spacing` of them, however many the cell had.
    if let Some(out_w) = function.value_type_opt(v).map(|t| t.bit_width())
        && out_w < 128
    {
        count = count.min((1u128 << out_w) / spacing.max(1)).max(1);
    }
    iv.count() == count
}

/// `1 << k`, or `None` past the `u128` carrier.
fn pow2(k: u128) -> Option<u128> {
    1u128.checked_shl(u32::try_from(k).ok()?)
}

#[cfg(test)]
#[path = "table_tests.rs"]
mod table_tests;
