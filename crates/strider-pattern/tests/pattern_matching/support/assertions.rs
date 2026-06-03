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

use strider_ir::Function;
use strider_ir::node::{NodeId, NodeKind};
use strider_pattern::pattern::Pattern;
use strider_pattern::{Match, Matcher};

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

/// Returns the first node in `function` whose kind satisfies `pred`,
/// panicking if none exists.
#[track_caller]
pub fn find_node<F: Fn(&NodeKind) -> bool>(function: &Function, pred: F) -> NodeId {
    function
        .walk()
        .find(|&n| pred(function.node_kind(n)))
        .expect("expected node kind not found in graph")
}
