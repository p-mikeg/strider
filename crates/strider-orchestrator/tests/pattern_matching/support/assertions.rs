use strider_ir::node::{NodeId, NodeKind};
use strider_ir::{Function, IRViewer, IRWalker};
use strider_pattern::{Match, Matcher, Pattern};

// Callers finalise their pattern before handing it to these helpers:
// typed value builders seal via `.into_pattern()`, control builders
// (`if_node()`, `phi_for()`, ...) via `.build()`. Accepting a finished
// `Pattern` keeps the helper signatures free of the match-vs-template
// trait split.

/// Panics with a descriptive message if `pat` doesn't match `expected` times.
#[track_caller]
pub(crate) fn matches(function: &Function, pat: Pattern, expected: usize) -> Vec<Match> {
    let hits = Matcher::new(function).find_all(&pat).unwrap();
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
pub(crate) fn unique(function: &Function, pat: Pattern) -> Match {
    let mut hits = matches(function, pat, 1);
    hits.pop().expect("unique requires exactly one match")
}

/// Asserts `pat` produces no matches.
#[track_caller]
pub(crate) fn none(function: &Function, pat: Pattern) {
    matches(function, pat, 0);
}

/// Asserts `pat` matches at least once and returns the first [`Match`].
///
/// Use this over [`unique`] when the graph may legitimately contain the same
/// shape more than once (e.g. a constant used twice) and the test only cares
/// that a match exists.
#[track_caller]
pub(crate) fn first(function: &Function, pat: Pattern) -> Match {
    let mut hits = Matcher::new(function).find_all(&pat).unwrap();
    assert!(!hits.is_empty(), "expected at least one match, got 0");
    hits.swap_remove(0)
}

/// Returns the first node whose kind satisfies `pred`, panicking if none exists.
#[track_caller]
pub(crate) fn find_node<F: Fn(&NodeKind) -> bool>(function: &Function, pred: F) -> NodeId {
    function
        .walk()
        .find(|&n| pred(function.node_kind(n)))
        .expect("expected node kind not found in graph")
}
