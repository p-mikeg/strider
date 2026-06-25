//! Assertion DSL for pattern-match tests.  Every test should end in one
//! of these helpers so failure messages are uniform and informative.
//!
//! The new bipartite API finalises a typed builder into a [`Pattern`] via
//! `.into_pattern()` (value-op structs) or `.build()` (control / memory
//! builders).  These helpers therefore take an already-finalised
//! [`Pattern`]; callers pass `pat.into_pattern()` / `builder.build()`.  If
//! a test needs a cast mask it sets it on the `Pattern` (via
//! [`Pattern::ignore_casts`] / [`Pattern::ignore_casts_mask`]) before
//! handing it over.

use strider_ir::node::{NodeId, NodeKind};
use strider_ir::{Function, IRViewer, IRWalker};
use strider_pattern::matcher::Pattern;
use strider_pattern::{Capture, Match, Matcher};

// ── Core assertions ───────────────────────────────────────────────────────────

/// Runs `pat` against `function` and returns the matches, panicking with
/// a descriptive message if the count differs from `expected`.
#[track_caller]
pub fn matches(function: &Function, pat: Pattern, expected: usize) -> Vec<Match> {
    let hits = Matcher::try_new(function).unwrap().find_all(&pat).unwrap();
    assert_eq!(
        hits.len(),
        expected,
        "expected {expected} match(es), got {}",
        hits.len()
    );
    hits
}

/// Asserts `pat` matches exactly once and returns that [`Match`].
#[track_caller]
pub fn unique(function: &Function, pat: Pattern) -> Match {
    let mut hits = matches(function, pat, 1);
    hits.pop().expect("unique requires exactly one match")
}

/// Asserts `pat` produces no matches.
#[track_caller]
pub fn none(function: &Function, pat: Pattern) {
    matches(function, pat, 0);
}

/// Asserts `pat` matches at least once and returns the first [`Match`].
///
/// Useful when the graph may legitimately contain the same shape in multiple
/// places (e.g. a constant used twice) but the test only cares about *any*
/// success, not exactly one.
#[track_caller]
pub fn first(function: &Function, pat: Pattern) -> Match {
    let mut hits = Matcher::try_new(function).unwrap().find_all(&pat).unwrap();
    assert!(!hits.is_empty(), "expected at least one match, got 0");
    hits.swap_remove(0)
}

/// Asserts a commutative shape matches in BOTH operand orders: each of
/// the two finished patterns (one per operand order) must match exactly
/// once.  Callers build the same pattern twice with the operands swapped;
/// non-commutative rejection / `.ordered()` cases stay with [`none`].
#[track_caller]
pub fn matches_both_orders(function: &Function, order_a: Pattern, order_b: Pattern) {
    matches(function, order_a, 1);
    matches(function, order_b, 1);
}

/// Asserts `pat` matches exactly once and reads capture `cap` back through
/// the match bindings as an unsigned integer constant (`None` when the
/// bound value is not an `IntConst`).
#[track_caller]
pub fn unique_uint(function: &Function, pat: Pattern, cap: Capture) -> Option<u128> {
    unique(function, pat).bindings().get_uint(cap, function)
}

/// Returns the first node in `function` whose kind satisfies `pred`,
/// panicking if none exists.
#[track_caller]
pub fn find_node<F: Fn(&NodeKind) -> bool>(function: &Function, pred: F) -> NodeId {
    function
        .walk()
        .find(|&n| pred(function.node_kind(n)))
        .expect("expected node kind not found in graph")
}
