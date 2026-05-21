//! `ControlState` walk-through helper for
//! [`MatcherOptions::ignore_control_states`].
//!
//! When the flag is set, the matcher's input-walking layer falls
//! through this helper if a direct match fails: instead of giving up,
//! it tries the inner pattern against each predecessor of the
//! region-join `ControlState`.
//!
//! Direct match is always tried first — the fallback runs only after a
//! direct attempt and bindings rollback, so strict patterns keep
//! matching unchanged.
//!
//! The sibling cast walk-through (selected by
//! [`MatcherOptions::ignore_cast_mask`]) lives inline in
//! [`crate::pattern::matcher::Matcher::match_output_with_walk_through`] — it
//! unwraps a value-passthrough cast and loops, which is cheaper as a
//! tail-loop than a recursive call.

use strider_ir::node::{NodeKind, NodeOutputId};

use crate::pattern::matcher::Bindings;
use crate::pattern::pat::Pat;
use crate::pattern::pat::traits::MatchCtx;

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
