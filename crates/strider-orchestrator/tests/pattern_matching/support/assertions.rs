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
