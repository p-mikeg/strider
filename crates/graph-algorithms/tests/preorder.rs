#![allow(
    clippy::panic,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::unreachable
)]

mod common;

use common::Graph;
use expect_test::expect;
use graph_algorithms::walk::entity_preorder;
use itertools::Itertools;

macro_rules! test_preorder {
    ($name:ident, $graph:literal, $expected:expr) => {
        #[test]
        fn $name() {
            let g = common::graph($graph);
            $expected.assert_eq(&collect_preorder(&g));
        }
    };
}

fn collect_preorder(g: &Graph) -> String {
    let preorder = entity_preorder(g, [g.entry()]);
    preorder.map(|node| g.name(node)).format(" ").to_string()
}

test_preorder! {
    straight_line,
    "a -> b
    b -> c
    c -> d",
    expect!["a b c d"]
}

test_preorder! {
    diamond,
    "a -> b, c
    b, c -> d",
    expect!["a c d b"]
}

test_preorder! {
    straight_line_skips,
    "a -> b, c
    b -> c, d
    c -> d
    d -> e",
    expect!["a c d e b"]
}

test_preorder! {
    simple_loop,
    "a -> b
    b -> c
    c -> b, e",
    expect!["a b c e"]
}

test_preorder! {
    loop_diamond,
    "a -> b
    b -> c, d
    c, d -> e
    e -> b, f",
    expect!["a b d e f c"]
}

#[test]
fn empty_roots_yields_nothing() {
    let g = common::graph("a -> b");
    assert!(entity_preorder(&g, core::iter::empty()).next().is_none());
}

#[test]
fn self_loop_visits_node_once() {
    let g = common::graph("a -> a");
    let order = entity_preorder(&g, [g.entry()])
        .map(|n| g.name(n).to_owned())
        .collect::<Vec<_>>();
    assert_eq!(order, vec!["a".to_owned()]);
}

#[test]
fn multi_root_disjoint_subgraphs_visits_both() {
    // Two disjoint chains: a -> b and x -> y. We pass both roots in.
    let g = common::graph(
        "a -> b
         x -> y",
    );
    let a = g.node("a");
    let x = g.node("x");
    let order = entity_preorder(&g, [a, x])
        .map(|n| g.name(n).to_owned())
        .collect::<Vec<_>>();
    assert_eq!(order.len(), 4);
    // Either {a, b} appears before {x, y} or vice versa, but each chain is
    // contiguous in pre-order. We assert both nodes from each chain appear.
    for name in ["a", "b", "x", "y"] {
        assert!(
            order.iter().any(|s| s == name),
            "missing {name} in {order:?}"
        );
    }
}

#[test]
fn repeated_successor_is_visited_once() {
    // a -> b appears twice as a successor of a (because we say so).  The pre-order
    // walk must still visit b exactly once.  This exercises the "skip if already
    // visited" loop in PreOrderContext::next.
    let g = common::graph(
        "a -> b, b
         b -> c",
    );
    let order = entity_preorder(&g, [g.entry()])
        .map(|n| g.name(n).to_owned())
        .collect::<Vec<_>>();
    assert_eq!(order.len(), 3);
    let count_b = order.iter().filter(|s| *s == "b").count();
    assert_eq!(count_b, 1);
}

#[test]
fn multi_root_visited_in_reverse_iteration_order() {
    // Doc-comment in PreOrderContext::reset promises: if `u` precedes `v` in
    // `roots` and there's no path from v to u, then `v` is visited before `u`
    // in pre-order (LIFO stack semantics — the OPPOSITE of post-order).
    // Build two disjoint chains (a -> b, x -> y) and pass roots in [a, x] order.
    let g = common::graph(
        "a -> b
         x -> y",
    );
    let a = g.node("a");
    let x = g.node("x");
    let order: Vec<_> = entity_preorder(&g, [a, x])
        .map(|n| g.name(n).to_owned())
        .collect();
    let pos_a = order.iter().position(|s| s == "a").unwrap();
    let pos_x = order.iter().position(|s| s == "x").unwrap();
    assert!(
        pos_x < pos_a,
        "expected x (second root) to precede a in pre-order \
         (LIFO root visit order), got {order:?}"
    );
}
