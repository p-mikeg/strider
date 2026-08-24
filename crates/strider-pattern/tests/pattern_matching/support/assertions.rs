//! These take an already-finalised [`Pattern`]: callers pass
//! `pat.into_pattern()` (value-op structs) or `builder.build()` (control /
//! memory builders), and set any cast mask on the `Pattern` first.

use strider_ir::node::{NodeId, NodeKind};
use strider_ir::{Function, IRViewer, IRWalker};
use strider_pattern::matcher::Pattern;
use strider_pattern::{Capture, Match, Matcher};

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

#[track_caller]
pub(crate) fn unique(function: &Function, pat: Pattern) -> Match {
    let mut hits = matches(function, pat, 1);
    hits.pop().expect("unique requires exactly one match")
}

#[track_caller]
pub(crate) fn none(function: &Function, pat: Pattern) {
    matches(function, pat, 0);
}

/// For graphs that legitimately hold the shape more than once, where
/// [`unique`] would over-constrain.
#[track_caller]
pub(crate) fn first(function: &Function, pat: Pattern) -> Match {
    let mut hits = Matcher::new(function).find_all(&pat).unwrap();
    assert!(!hits.is_empty(), "expected at least one match, got 0");
    hits.swap_remove(0)
}

/// Asserts a commutative shape matches in BOTH operand orders. Callers build
/// the same pattern twice with operands swapped; `.ordered()` rejection cases
/// use [`none`] instead.
#[track_caller]
pub(crate) fn matches_both_orders(function: &Function, order_a: Pattern, order_b: Pattern) {
    matches(function, order_a, 1);
    matches(function, order_b, 1);
}

/// Reads `cap` back as an unsigned constant. `None` when the bound value is
/// not an `IntConst`.
#[track_caller]
pub(crate) fn unique_uint(function: &Function, pat: Pattern, cap: Capture) -> Option<u128> {
    unique(function, pat).bindings().get_uint(cap, function)
}

#[track_caller]
pub(crate) fn find_node<F: Fn(&NodeKind) -> bool>(function: &Function, pred: F) -> NodeId {
    function
        .walk()
        .find(|&n| pred(function.node_kind(n)))
        .expect("expected node kind not found in graph")
}
