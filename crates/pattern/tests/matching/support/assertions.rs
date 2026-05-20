//! Assertion DSL for pattern-match tests.  Every test should end in one
//! of these helpers so failure messages are uniform and informative.

use strider_ir::BuiltFunctionGraph;
use strider_ir::node::{NodeId, NodeKind};
use pattern::{Match, Matcher, Pat};

// ── Core assertions ───────────────────────────────────────────────────────────

/// Runs `pat` against `g` and returns the expected number of matches,
/// panicking with a descriptive message otherwise.
#[track_caller]
pub fn matches(g: &BuiltFunctionGraph, pat: impl Into<Pat>, expected: usize) -> Vec<Match> {
    let pat = pat.into();
    let hits = Matcher::new(g).find_all(&pat);
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
pub fn unique(g: &BuiltFunctionGraph, pat: impl Into<Pat>) -> Match {
    let mut hits = matches(g, pat, 1);
    hits.pop().expect("unique requires exactly one match")
}

/// Asserts `pat` produces no matches.
#[track_caller]
pub fn none(g: &BuiltFunctionGraph, pat: impl Into<Pat>) {
    matches(g, pat, 0);
}

/// Asserts `pat` matches at least once and returns the first [`Match`].
///
/// Useful when the graph may legitimately contain the same shape in multiple
/// places (e.g. a constant used twice) but the test only cares about *any*
/// success, not exactly one.
#[track_caller]
pub fn first(g: &BuiltFunctionGraph, pat: impl Into<Pat>) -> Match {
    let pat = pat.into();
    let mut hits = Matcher::new(g).find_all(&pat);
    assert!(!hits.is_empty(), "expected at least one match, got 0");
    hits.swap_remove(0)
}

/// Returns the first node in `g` whose kind satisfies `pred`, panicking if
/// none exists.  Used by rewrite tests to pick a specific root to feed into
/// `rewrite_rule`.
#[track_caller]
pub fn find_node<F: Fn(&NodeKind) -> bool>(g: &BuiltFunctionGraph, pred: F) -> NodeId {
    g.preorder()
        .find(|&n| pred(g.graph.node_kind(n)))
        .expect("expected node kind not found in graph")
}
