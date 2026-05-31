//! `Region` walk-through helper for
//! [`MatcherOptions::ignore_regions`].
//!
//! When the flag is set, the matcher's input-walking layer falls
//! through this helper if a direct match fails: instead of giving up,
//! it tries the inner pattern against each predecessor of the
//! region-join `Region`.
//!
//! Direct match is always tried first — the fallback runs only after a
//! direct attempt and bindings rollback, so strict patterns keep
//! matching unchanged.
//!
//! The structural enumeration of predecessors lives in
//! [`strider_ir::walk::region_predecessors`]; this helper owns
//! only the per-attempt bindings-rollback policy and the recursion back
//! into the matcher.
//!
//! The sibling cast walk-through (selected by
//! [`MatcherOptions::ignore_cast_mask`]) lives inline in
//! [`crate::pattern::matcher::Matcher::match_output_with_walk_through`] — it
//! unwraps a value-passthrough cast and loops, which is cheaper as a
//! tail-loop than a recursive call.

use strider_ir::node::NodeOutputId;
use strider_ir::walk::region_predecessors;

use crate::pattern::matcher::Bindings;
use crate::pattern::pat::Pat;
use crate::pattern::pat::traits::MatchCtx;

/// Backward walk-through of a `Region` (region-join) node.  If
/// `target`'s producer is a `Region`, try matching `pat` against
/// each of the Region's control-typed inputs (one per
/// predecessor region).  Returns true on first success.
///
/// Iterates predecessors via the structural enumerator in
/// `strider_ir::walk` and rolls back bindings between failed attempts
/// via `b.mark()` / `b.restore()`.  Recurses via the walk-through entry
/// point so chained Regions (region joins of region joins) also
/// resolve.
///
/// Used to implement `ret(call(...))` against IR shapes where a region
/// join (`Return ← Region ← Call`) sits between the Return and
/// the Call — the strict matcher would fail because `Return.input[0]`
/// is the Region, not the Call directly.
#[must_use]
pub(crate) fn try_walk_through_region(
    ctx: &MatchCtx,
    target: NodeOutputId,
    pat: &Pat,
    b: &mut Bindings,
) -> bool {
    let mark = b.mark();
    for input in region_predecessors(ctx.function, target) {
        if ctx.matcher.match_output_with_walk_through(input, pat, b) {
            return true;
        }
        b.restore(mark);
    }
    false
}

#[cfg(test)]
mod tests {
    //! White-box tests for `try_walk_through_region` and the
    //! `ignore_regions` path it implements.
    //!
    //! These tests construct realistic multi-region IRs and exercise
    //! the walk-through helper through the public `Matcher` API
    //! (`Matcher::try_new(...).ignore_regions()`).  Direct
    //! invocation isn't useful here — the helper needs a `MatchCtx`
    //! anchored on a real `Matcher`, and the matcher's own
    //! `match_output_with_walk_through` is the only legitimate caller.

    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use crate::pattern::{call, ret, Matcher, Pat};
    use strider_ir::node::{NodeKind, NodeOutputType};
    use strider_ir_test_utils::RegisterSet;

    /// Single-region function whose Return has no preceding Region
    /// join — `try_walk_through_region` is called against a non-CS
    /// producer and must return false (the `take(0)` branch).
    #[test]
    fn no_region_input_returns_false() {
        let mut b = RegisterSet::new().build_fn_single_region().unwrap();
        let v = b.build_int_const(0xCAFEu64, NodeOutputType::I64).unwrap();
        b.build_return(Some(v), &[]).unwrap();
        b.set_lift_addr(None);
        let fg = b.build().unwrap();

        // Without a Region between Return and the inner value,
        // `ret().preceded_by(call())` shouldn't match (no Call exists)
        // even with walk-through enabled.  Pins the "non-CS producer →
        // no fan-out" behaviour.
        let pat: Pat = ret().preceded_by(call()).into();
        let hits = Matcher::try_new(&fg).unwrap()
            .ignore_regions()
            .find_all(&pat);
        assert!(hits.is_empty(), "no Call in graph: no match");
    }

    /// Single-predecessor Region (a region join with one input).
    /// The walk-through tries the lone predecessor and that's it.
    #[test]
    fn single_predecessor_region_walks_through() {
        // entry: Call → branch to tail.  tail: Return (single predecessor
        // Region at tail).  ret().preceded_by(call()) must match through the
        // walk-through.
        let mut b = RegisterSet::new().build_fn().unwrap();
        let head = b.create_region().unwrap();
        let tail = b.create_region().unwrap();
        b.set_entry_region(head).unwrap();
        b.set_region(head);
        let target = b.build_int_const(0xCAFEu64, NodeOutputType::I64).unwrap();
        b.build_call(target).unwrap();
        b.build_branch(tail).unwrap();
        b.set_region(tail);
        b.build_return(None, &[]).unwrap();
        b.set_lift_addr(None);
        let fg = b.build().unwrap();

        let pat: Pat = ret().preceded_by(call()).into();
        let hits = Matcher::try_new(&fg).unwrap()
            .ignore_regions()
            .find_all(&pat);
        assert_eq!(hits.len(), 1, "ret(call) through 1-pred Region must match");
    }

    /// Two-predecessor Region where both predecessors are Calls.
    /// The walk-through tries each predecessor — first success wins, so
    /// exactly one match is reported per Return.
    #[test]
    fn n_predecessors_first_match_wins() {
        // entry: branch on cond to a or b.
        // a: Call(0xA) → branch to join.
        // b: Call(0xB) → branch to join.
        // join: Return.
        let mut b = RegisterSet::new().build_fn().unwrap();
        let entry = b.create_region().unwrap();
        let arm_a = b.create_region().unwrap();
        let arm_b = b.create_region().unwrap();
        let join = b.create_region().unwrap();
        b.set_entry_region(entry).unwrap();

        b.set_region(entry);
        let cond = b.build_boolean_const(true);
        b.build_if(cond, arm_a, arm_b).unwrap();

        b.set_region(arm_a);
        let ta = b.build_int_const(0xAu64, NodeOutputType::I64).unwrap();
        b.build_call(ta).unwrap();
        b.build_branch(join).unwrap();

        b.set_region(arm_b);
        let tb = b.build_int_const(0xBu64, NodeOutputType::I64).unwrap();
        b.build_call(tb).unwrap();
        b.build_branch(join).unwrap();

        b.set_region(join);
        b.build_return(None, &[]).unwrap();
        b.set_lift_addr(None);
        let fg = b.build().unwrap();

        let pat: Pat = ret().preceded_by(call()).into();
        let hits = Matcher::try_new(&fg).unwrap()
            .ignore_regions()
            .find_all(&pat);
        assert_eq!(
            hits.len(),
            1,
            "first-match wins: exactly one ret() hit",
        );
    }

    /// Two-predecessor Region where ONLY one arm has a Call —
    /// the other arm just branches through.  The walk-through must
    /// keep trying after the first failure and find the second arm.
    /// Pins the rollback-between-failures contract.
    #[test]
    fn n_predecessors_only_one_matches_walkthrough_rolls_back() {
        // entry: branch on cond to a (call arm) or b (no-call arm).
        // a: Call(...) → branch to join.
        // b: no-op → branch to join.
        // join: Return.
        let mut b = RegisterSet::new().build_fn().unwrap();
        let entry = b.create_region().unwrap();
        let arm_a = b.create_region().unwrap();
        let arm_b = b.create_region().unwrap();
        let join = b.create_region().unwrap();
        b.set_entry_region(entry).unwrap();

        b.set_region(entry);
        let cond = b.build_boolean_const(true);
        b.build_if(cond, arm_a, arm_b).unwrap();

        b.set_region(arm_a);
        let ta = b.build_int_const(0xAu64, NodeOutputType::I64).unwrap();
        b.build_call(ta).unwrap();
        b.build_branch(join).unwrap();

        b.set_region(arm_b);
        b.build_branch(join).unwrap();

        b.set_region(join);
        b.build_return(None, &[]).unwrap();
        b.set_lift_addr(None);
        let fg = b.build().unwrap();

        let pat: Pat = ret().preceded_by(call()).into();
        let hits = Matcher::try_new(&fg).unwrap()
            .ignore_regions()
            .find_all(&pat);
        // One Return, but the walk-through must reach into the Call arm
        // (after failing on the branch-through arm) — exactly one match.
        assert_eq!(hits.len(), 1, "rollback then success on the other arm");
    }

    /// Without `ignore_regions`, the same multi-region graph
    /// fails to match `ret().preceded_by(call())` even when a Call is
    /// reachable upstream through the Region — confirms the helper is only
    /// engaged when the flag is set.
    #[test]
    fn flag_off_disables_walk_through() {
        // Reuse the single-predecessor shape from above.
        let mut b = RegisterSet::new().build_fn().unwrap();
        let head = b.create_region().unwrap();
        let tail = b.create_region().unwrap();
        b.set_entry_region(head).unwrap();
        b.set_region(head);
        let target = b.build_int_const(0xCAFEu64, NodeOutputType::I64).unwrap();
        b.build_call(target).unwrap();
        b.build_branch(tail).unwrap();
        b.set_region(tail);
        b.build_return(None, &[]).unwrap();
        b.set_lift_addr(None);
        let fg = b.build().unwrap();

        let pat: Pat = ret().preceded_by(call()).into();
        // No `.ignore_regions()` — direct match through the Region
        // fails, and the helper is gated off.
        let hits = Matcher::try_new(&fg).unwrap().find_all(&pat);
        assert!(hits.is_empty(), "flag off: no walk-through");
    }

    /// Chained Regions: head → mid (Region via branch) → tail (Region via
    /// branch).  The walk-through must recurse through nested joins.
    /// Confirms the recursive walk via `match_output_with_walk_through`
    /// reaches the upstream Call across multiple region joins.
    #[test]
    fn chained_controlstates_walk_through() {
        let mut b = RegisterSet::new().build_fn().unwrap();
        let head = b.create_region().unwrap();
        let mid = b.create_region().unwrap();
        let tail = b.create_region().unwrap();
        b.set_entry_region(head).unwrap();

        b.set_region(head);
        let target = b.build_int_const(0xC0FFEEu64, NodeOutputType::I64).unwrap();
        b.build_call(target).unwrap();
        b.build_branch(mid).unwrap();

        b.set_region(mid);
        b.build_branch(tail).unwrap();

        b.set_region(tail);
        b.build_return(None, &[]).unwrap();
        b.set_lift_addr(None);
        let fg = b.build().unwrap();

        // Sanity: at least two Region nodes are reachable (mid +
        // tail) before we run the matcher.
        let cs_count = fg
            .walk()
            .filter(|&n| matches!(fg.node_kind(n), NodeKind::Region))
            .count();
        assert!(cs_count >= 2, "chain produces >=2 Region nodes (got {cs_count})");

        let pat: Pat = ret().preceded_by(call()).into();
        let hits = Matcher::try_new(&fg).unwrap()
            .ignore_regions()
            .find_all(&pat);
        assert_eq!(hits.len(), 1, "chained Region walk-through must reach Call");
    }
}
