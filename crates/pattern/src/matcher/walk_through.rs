//! Walk-through helpers for [`MatcherOptions::ignore_casts`] and
//! [`MatcherOptions::ignore_control_states`].
//!
//! When a flag is set, the matcher's input-walking layer falls through
//! these helpers if a direct match fails: instead of giving up, it
//! recurses past a "transparent" producer node (a value-passthrough cast,
//! or a region-join `ControlState`) and retries the inner pattern.
//!
//! Direct match is always tried first — the fallback runs only after a
//! direct attempt and bindings rollback, so strict patterns (like
//! `truncate(x)` looking for a literal Truncate) keep matching unchanged.

use ir::node::{NodeKind, NodeOutputId};

use crate::matcher::Bindings;
use crate::pat::Pat;
use crate::pat::traits::MatchCtx;

/// Returns `true` for value-passthrough cast kinds — node kinds that take
/// a single value input and produce a value output of potentially
/// different width / type but conceptually carry "the same value" along
/// the data-flow chain.
///
/// The set covers width casts (`Extend`, `Truncate`), type casts
/// (`CastToInt`, `CastToFloat`, `CastToBool`), and bitcasts
/// (`IntBitsToFloat`, `FloatBitsToInt`).  Future cast-like additions to
/// `NodeKind` should extend this match — the exhaustive form (no `_`
/// catch-all) makes the omission a compile error.
#[must_use]
pub(crate) fn is_cast_kind(kind: &NodeKind) -> bool {
    match kind {
        NodeKind::Extend(_)
        | NodeKind::Truncate
        | NodeKind::CastToInt
        | NodeKind::CastToFloat
        | NodeKind::CastToBool
        | NodeKind::IntBitsToFloat
        | NodeKind::FloatBitsToInt => true,

        // Explicit non-cast list: every other NodeKind variant.  The
        // exhaustive `match` (no `_`) catches future cast-like additions
        // at compile time so a new cast-like kind doesn't silently miss
        // the walk-through.
        NodeKind::Entry
        | NodeKind::InitialMemory
        | NodeKind::InitialVar(_)
        | NodeKind::FunctionArg { .. }
        | NodeKind::ControlState
        | NodeKind::ControlPhi(_)
        | NodeKind::MemPhi
        | NodeKind::ValuePhi
        | NodeKind::If
        | NodeKind::Call
        | NodeKind::CallOther { .. }
        | NodeKind::Return
        | NodeKind::PostCallMemState
        | NodeKind::PostCallVarState(_)
        | NodeKind::Load(_)
        | NodeKind::Store(_)
        | NodeKind::StackStore { .. }
        | NodeKind::StackStorePhi { .. }
        | NodeKind::IntConst(_)
        | NodeKind::IntUnaryOp(_)
        | NodeKind::IntBinaryOp(_)
        | NodeKind::IntCmpOp(_)
        | NodeKind::Popcount
        | NodeKind::Lzcount
        | NodeKind::BoolConst(_)
        | NodeKind::BoolUnaryOp(_)
        | NodeKind::BoolBinaryOp(_)
        | NodeKind::FloatConst(_)
        | NodeKind::FloatUnaryOp(_)
        | NodeKind::FloatBinaryOp(_)
        | NodeKind::FloatCmpOp(_)
        | NodeKind::IntToFloat
        | NodeKind::FloatToInt
        | NodeKind::FloatToFloat
        | NodeKind::SegmentOp { .. }
        | NodeKind::CPoolRef
        | NodeKind::New => false,
    }
}

/// If `target`'s producer is a cast node, recurse into the cast's first
/// (value) input and try matching `pat` there.  Returns whether the
/// recursive match succeeded.
///
/// Caller is responsible for snapshotting bindings (`b.mark()`) before
/// the call and rolling back on failure — this helper does NOT manage
/// rollback because the snapshot is shared with the direct-match attempt
/// in `match_one`.
///
/// Returns `false` immediately if the producer is not a cast or the cast
/// has no inputs (defensive — every cast in our IR has exactly one
/// value input by signature).
#[must_use]
pub(crate) fn try_walk_through_cast(
    ctx: &MatchCtx,
    target: NodeOutputId,
    pat: &Pat,
    b: &mut Bindings,
) -> bool {
    let producer = ctx.graph.graph.get_node_from_output(target);
    if !is_cast_kind(ctx.graph.graph.node_kind(producer)) {
        return false;
    }
    // Casts have exactly one input; treat any unexpected shape as
    // "can't walk through" rather than panicking.
    let inputs = ctx.graph.graph.node_inputs(producer);
    let Some(value_input) = inputs.into_iter().next() else {
        return false;
    };
    // Recurse via the walk-through entry point so chained casts
    // (e.g. `Extend(Truncate(Mul))`) also resolve.
    ctx.matcher.match_output_with_walk_through(value_input, pat, b)
}

/// Backward walk-through of a `ControlState` (region-join) node.  If
/// `target`'s producer is a `ControlState`, try matching `pat` against
/// each of the ControlState's control-typed inputs (one per
/// predecessor region).  Returns true on first success.
///
/// `ControlState`'s signature is `inputs: variadic Control; outputs:
/// [Control, ControlPhi]`, so every input is a control-typed producer
/// from a predecessor region.  This helper tries them in order and
/// rolls back bindings between attempts via `b.mark()` / `b.restore()`.
///
/// Used to implement `ret(call(...))` against IR shapes where a region
/// join (`Return ← ControlState ← Call`) sits between the Return and
/// the Call — the strict matcher would fail because `Return.input[0]`
/// is the ControlState, not the Call directly.
#[must_use]
pub(crate) fn try_walk_through_control_state(
    ctx: &MatchCtx,
    target: NodeOutputId,
    pat: &Pat,
    b: &mut Bindings,
) -> bool {
    let producer = ctx.graph.graph.get_node_from_output(target);
    if !matches!(ctx.graph.graph.node_kind(producer), NodeKind::ControlState) {
        return false;
    }
    // Try each control input; rollback bindings between failed attempts.
    // Recurse via the walk-through entry point so chained ControlStates
    // (region joins of region joins) also resolve.
    let mark = b.mark();
    for input in ctx.graph.graph.node_inputs(producer) {
        if ctx.matcher.match_output_with_walk_through(input, pat, b) {
            return true;
        }
        b.restore(mark);
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use ir::ExtendOp;

    /// All seven value-passthrough cast kinds must classify as casts.
    #[test]
    fn is_cast_kind_returns_true_for_all_cast_kinds() {
        let casts = [
            NodeKind::Extend(ExtendOp::ZeroExtend),
            NodeKind::Extend(ExtendOp::SignExtend),
            NodeKind::Truncate,
            NodeKind::CastToInt,
            NodeKind::CastToFloat,
            NodeKind::CastToBool,
            NodeKind::IntBitsToFloat,
            NodeKind::FloatBitsToInt,
        ];
        for k in casts {
            assert!(
                is_cast_kind(&k),
                "expected is_cast_kind({k:?}) = true"
            );
        }
    }

    /// A representative selection of non-cast kinds must classify as
    /// non-casts.  (Exhaustive coverage is enforced by the no-`_`
    /// match in `is_cast_kind` itself — adding a NodeKind variant
    /// without classifying it is a compile error.)
    #[test]
    fn is_cast_kind_returns_false_for_non_cast_kinds() {
        let non_casts = [
            NodeKind::Entry,
            NodeKind::IntConst(0),
            NodeKind::IntBinaryOp(ir::IntBinaryOp::Add),
            NodeKind::IntBinaryOp(ir::IntBinaryOp::Mul),
            NodeKind::IntUnaryOp(ir::IntUnaryOp::Neg),
            NodeKind::BoolConst(true),
            NodeKind::FloatBinaryOp(ir::FloatBinaryOp::Add),
            NodeKind::FloatToFloat,
            NodeKind::FloatToInt,
            NodeKind::IntToFloat,
            NodeKind::Return,
            NodeKind::ControlState,
            NodeKind::MemPhi,
            NodeKind::If,
            NodeKind::Call,
        ];
        for k in non_casts {
            assert!(
                !is_cast_kind(&k),
                "expected is_cast_kind({k:?}) = false"
            );
        }
    }

    /// Sanity check on FloatToFloat / FloatToInt / IntToFloat — these
    /// are float **conversions** (semantic value change), not bit-level
    /// casts.  They must NOT be in the walk-through set: a pattern
    /// looking for a Mul should not silently match through a
    /// FloatToInt that semantically changed the value.
    #[test]
    fn is_cast_kind_excludes_float_conversions() {
        assert!(!is_cast_kind(&NodeKind::FloatToFloat));
        assert!(!is_cast_kind(&NodeKind::FloatToInt));
        assert!(!is_cast_kind(&NodeKind::IntToFloat));
    }
}
