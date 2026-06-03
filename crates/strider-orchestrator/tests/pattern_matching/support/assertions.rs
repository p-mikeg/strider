//! Assertion DSL for pattern-match tests.  Every test should end in one
//! of these helpers so failure messages are uniform and informative.

use strider_ir::Function;
use strider_ir::node::{NodeId, NodeKind};
use strider_pattern::{Match, Matcher, Pattern};

// Callers finalise their pattern before handing it to these helpers:
// typed value builders seal via `.into_pattern()`, control builders
// (`if_node()`, `phi_for()`, …) via `.build()`. Accepting a finished
// `Pattern` keeps the helper signatures free of the match-vs-template
// trait split.

// ── Core assertions ───────────────────────────────────────────────────────────

/// Runs `pat` against `g` and returns the matches, panicking with a
/// descriptive message if the count differs from `expected`.
#[track_caller]
pub fn matches(function: &Function, pat: Pattern, expected: usize) -> Vec<Match> {
    let hits = Matcher::try_new(function).unwrap().find_all(&pat);
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
    let mut hits = Matcher::try_new(function).unwrap().find_all(&pat);
    assert!(!hits.is_empty(), "expected at least one match, got 0");
    hits.swap_remove(0)
}

/// Returns the first node in `g` whose kind satisfies `pred`, panicking if
/// none exists.
#[track_caller]
pub fn find_node<F: Fn(&NodeKind) -> bool>(function: &Function, pred: F) -> NodeId {
    function
        .walk()
        .find(|&n| pred(function.node_kind(n)))
        .expect("expected node kind not found in graph")
}
