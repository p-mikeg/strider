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
//! The structural enumeration of predecessors lives in
//! [`strider_ir::walk::control_state_predecessors`]; this helper owns
//! only the per-attempt bindings-rollback policy and the recursion back
//! into the matcher.
//!
//! The sibling cast walk-through (selected by
//! [`MatcherOptions::ignore_cast_mask`]) lives inline in
//! [`crate::pattern::matcher::Matcher::match_output_with_walk_through`] — it
//! unwraps a value-passthrough cast and loops, which is cheaper as a
//! tail-loop than a recursive call.

use strider_ir::node::NodeOutputId;
use strider_ir::walk::control_state_predecessors;

use crate::pattern::matcher::Bindings;
use crate::pattern::pat::Pat;
use crate::pattern::pat::traits::MatchCtx;

/// Backward walk-through of a `ControlState` (region-join) node.  If
/// `target`'s producer is a `ControlState`, try matching `pat` against
/// each of the ControlState's control-typed inputs (one per
/// predecessor region).  Returns true on first success.
///
/// Iterates predecessors via the structural enumerator in
/// `strider_ir::walk` and rolls back bindings between failed attempts
/// via `b.mark()` / `b.restore()`.  Recurses via the walk-through entry
/// point so chained ControlStates (region joins of region joins) also
/// resolve.
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
    let mark = b.mark();
    for input in control_state_predecessors(ctx.graph, target) {
        if ctx.matcher.match_output_with_walk_through(input, pat, b) {
            return true;
        }
        b.restore(mark);
    }
    false
}
