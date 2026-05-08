//! Walk-through helpers for [`MatcherOptions::ignore_cast_mask`] and
//! [`MatcherOptions::ignore_control_states`].
//!
//! When a flag / mask bit is set, the matcher's input-walking layer falls
//! through these helpers if a direct match fails: instead of giving up,
//! it recurses past a "transparent" producer node (a value-passthrough
//! cast selected by the mask, or a region-join `ControlState`) and
//! retries the inner pattern.
//!
//! Direct match is always tried first — the fallback runs only after a
//! direct attempt and bindings rollback, so strict patterns (like
//! `truncate(x)` looking for a literal Truncate) keep matching unchanged.

use ir::node::{NodeKind, NodeOutputId};

use crate::matcher::Bindings;
use crate::pat::Pat;
use crate::pat::traits::MatchCtx;

/// If `target`'s producer is a cast node selected by
/// `ctx.matcher.options.ignore_cast_mask`, recurse into the cast's first
/// (value) input and try matching `pat` there.  Returns whether the
/// recursive match succeeded.
///
/// Caller is responsible for snapshotting bindings (`b.mark()`) before
/// the call and rolling back on failure — this helper does NOT manage
/// rollback because the snapshot is shared with the direct-match attempt
/// in `match_one`.
///
/// Backward walk-through of a `ControlState` (region-join) node.  If
/// `target`'s producer is a `ControlState`, try matching `pat` against
/// each of the ControlState's control-typed inputs (one per
/// predecessor region).  Returns true on first success.
///
/// `ControlState`'s signature is `inputs: variadic Control; outputs:
/// [Control, PhiToken]`, so every input is a control-typed producer
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
    let producer = ctx.graph.get_node_from_output(target);
    if !matches!(ctx.graph.node_kind(producer), NodeKind::ControlState) {
        return false;
    }
    // Try each control input; rollback bindings between failed attempts.
    // Recurse via the walk-through entry point so chained ControlStates
    // (region joins of region joins) also resolve.
    let mark = b.mark();
    for input in ctx.graph.node_inputs(producer) {
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
    use crate::matcher::cast_mask::{CastMask, cast_mask_of};
    use ir::ExtendOp;

    /// All eight value-passthrough cast kinds must yield a non-empty mask.
    #[test]
    fn cast_mask_of_returns_non_empty_for_all_cast_kinds() {
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
                !cast_mask_of(&k).is_empty(),
                "expected cast_mask_of({k:?}) to be non-empty"
            );
        }
    }

    /// A representative selection of non-cast kinds must yield empty.
    /// (Exhaustive coverage is enforced by the no-`_` match in
    /// `cast_mask_of` itself — adding a NodeKind variant without
    /// classifying it is a compile error.)
    #[test]
    fn cast_mask_of_returns_empty_for_non_cast_kinds() {
        let non_casts = [
            NodeKind::Entry,
            NodeKind::IntConst(0),
            NodeKind::IntBinaryOp(ir::IntBinaryOp::Add),
            NodeKind::IntBinaryOp(ir::IntBinaryOp::Mul),
            NodeKind::IntUnaryOp(ir::IntUnaryOp::BitNot),
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
            assert_eq!(
                cast_mask_of(&k),
                CastMask::empty(),
                "expected cast_mask_of({k:?}) = empty"
            );
        }
    }

    /// Sanity check on FloatToFloat / FloatToInt / IntToFloat — these
    /// are float **conversions** (semantic value change), not bit-level
    /// casts.  They must NOT be in the walk-through set: a pattern
    /// looking for a Mul should not silently match through a
    /// FloatToInt that semantically changed the value.
    #[test]
    fn cast_mask_of_excludes_float_conversions() {
        assert_eq!(cast_mask_of(&NodeKind::FloatToFloat), CastMask::empty());
        assert_eq!(cast_mask_of(&NodeKind::FloatToInt), CastMask::empty());
        assert_eq!(cast_mask_of(&NodeKind::IntToFloat), CastMask::empty());
    }
}
