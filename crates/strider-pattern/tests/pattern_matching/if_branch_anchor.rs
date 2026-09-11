//! A branch pattern anchors on the consumer's outputs only until one of them
//! is a repeat of another, so a failing walk runs once rather than once per
//! output, and nesting adds a level instead of doubling everything below it.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use strider_ir::node::NodeKind;
use strider_pattern::matcher::{KindSpec, MatcherBuilder};
use strider_pattern::{MatchPat, Matcher, Pattern, if_else};

use super::support::shapes;

/// `Region`-rooted, with a vertex per output so each of the consumer's two
/// outputs resolves to its own. `inner` nests one more branch walk beneath it;
/// without one the root counts its attempts and rejects.
fn counted_region(calls: &Arc<AtomicUsize>, inner: Option<Pattern>) -> Pattern {
    let mut b = MatcherBuilder::new();
    let n = b.node(KindSpec::Exact(NodeKind::Region));
    b.control_output(n, 0);
    let phi = b.value_output(n, 1);
    b.set_output_any(phi);
    match inner {
        Some(p) => {
            let sub = if_else().with_false(p).compile(&mut b);
            b.input(n, 0, sub);
        }
        None => {
            let calls = Arc::clone(calls);
            b.set_node_predicate_at(
                n,
                Box::new(move |_m, _n| {
                    calls.fetch_add(1, Ordering::Relaxed);
                    false
                }),
            );
        }
    }
    b.finish()
}

/// The two attempts differ only in which vertex anchors, so the second cannot
/// reach the continuation the first did not.
#[test]
fn a_failing_branch_walk_runs_once_per_consumer_not_once_per_output() {
    let f = shapes::if_cmp_then_return(4);
    let m = Matcher::new(&f);
    let calls = Arc::new(AtomicUsize::new(0));
    let pat = if_else().with_false(counted_region(&calls, None)).build();

    assert!(m.find_all(&pat).unwrap().is_empty());
    assert_eq!(
        calls.load(Ordering::Relaxed),
        2,
        "one attempt per output of the `If` the query is rooted at"
    );
}

/// The cost of a failing chain is what pins this: a per-level retry is 2^D.
#[test]
fn nesting_branch_patterns_does_not_double_the_failure_cost() {
    for depth in [0usize, 4, 12] {
        let f = shapes::if_cmp_then_return(4);
        let m = Matcher::new(&f);
        let calls = Arc::new(AtomicUsize::new(0));
        let mut branch = counted_region(&calls, None);
        for _ in 0..depth {
            branch = counted_region(&calls, Some(branch));
        }
        let pat = if_else().with_false(branch).build();

        assert!(m.find_all(&pat).unwrap().is_empty());
        assert_eq!(
            calls.load(Ordering::Relaxed),
            2,
            "depth {depth} must cost what depth 0 does"
        );
    }
}
