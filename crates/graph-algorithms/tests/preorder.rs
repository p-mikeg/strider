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
    // Chain order is asserted separately; here only reachability matters.
    for name in ["a", "b", "x", "y"] {
        assert!(
            order.iter().any(|s| s == name),
            "missing {name} in {order:?}"
        );
    }
}

#[test]
fn repeated_successor_is_visited_once() {
    // `b` is listed twice as a successor of `a`, so it reaches the stack twice.
    // Exercises the skip-if-visited loop in PreOrderContext::next.
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
    // Pins the LIFO root order PreOrderContext::reset documents: roots [a, x]
    // over disjoint chains must visit x's chain first (opposite of post-order).
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
