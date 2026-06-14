#![allow(
    clippy::panic,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::unreachable
)]

mod common;

use common::Graph;
use expect_test::expect;
use graphwalk::{WalkPhase, entity_postorder};
use itertools::Itertools;
use std::fmt::Write;

macro_rules! test_postorder {
    ($name:ident, $graph:literal, $expected_full:expr, $expected_rpo:expr) => {
        #[test]
        fn $name() {
            let g = common::graph($graph);
            let (full, rpo) = collect_postorder(&g);
            $expected_full.assert_eq(&full);
            $expected_rpo.assert_eq(&rpo);
        }
    };
}

fn collect_postorder(g: &Graph) -> (String, String) {
    let mut postorder = entity_postorder(g, [g.entry()]);

    let mut full = String::new();
    let mut indent = 0;
    while let Some((phase, node)) = postorder.next_event() {
        let (phase, indent) = match phase {
            WalkPhase::Pre => {
                let cur_indent = indent;
                indent += 1;
                ("ent", cur_indent)
            }
            WalkPhase::Post => {
                indent -= 1;
                ("ex", indent)
            }
        };
        writeln!(full, "{:indent$}{phase}:{}", "", g.name(node)).unwrap();
    }

    let mut rpo: Vec<_> = entity_postorder(g, [g.entry()]).collect();
    rpo.reverse();
    let rpo = rpo.iter().map(|&node| g.name(node)).format(" ").to_string();

    (full, rpo)
}

test_postorder! {
    straight_line,
    "a -> b
    b -> c
    c -> d",
    expect![[r"
        ent:a
         ent:b
          ent:c
           ent:d
           ex:d
          ex:c
         ex:b
        ex:a
    "]],
    expect!["a b c d"]
}

test_postorder! {
    diamond,
    "a -> b, c
    b, c -> d",
    expect![[r"
        ent:a
         ent:c
          ent:d
          ex:d
         ex:c
         ent:b
         ex:b
        ex:a
    "]],
    expect!["a b c d"]
}

test_postorder! {
    straight_line_skips,
    "a -> b, c
    b -> c, d
    c -> d
    d -> e",
    expect![[r"
        ent:a
         ent:c
          ent:d
           ent:e
           ex:e
          ex:d
         ex:c
         ent:b
         ex:b
        ex:a
    "]],
    expect!["a b c d e"]
}

test_postorder! {
    simple_loop,
    "a -> b
    b -> c
    c -> b, e",
    expect![[r"
        ent:a
         ent:b
          ent:c
           ent:e
           ex:e
          ex:c
         ex:b
        ex:a
    "]],
    expect!["a b c e"]
}

test_postorder! {
    loop_diamond,
    "a -> b
    b -> c, d
    c, d -> e
    e -> b, f",
    expect![[r"
        ent:a
         ent:b
          ent:d
           ent:e
            ent:f
            ex:f
           ex:e
          ex:d
          ent:c
          ex:c
         ex:b
        ex:a
    "]],
    expect!["a b c d e f"]
}

#[test]
fn empty_roots_yields_nothing() {
    let g = common::graph("a -> b");
    let mut po = entity_postorder(&g, core::iter::empty());
    assert!(po.next().is_none());
    assert!(po.next_event().is_none());
}

#[test]
fn self_loop_emits_pre_and_post_once() {
    let g = common::graph("a -> a");
    let mut po = entity_postorder(&g, [g.entry()]);
    let events: Vec<_> = core::iter::from_fn(|| po.next_event()).collect();
    assert_eq!(events.len(), 2);
    assert!(matches!(events[0].0, WalkPhase::Pre));
    assert!(matches!(events[1].0, WalkPhase::Post));
    assert_eq!(events[0].1, events[1].1);
}

#[test]
fn multi_root_preserves_root_order_in_rpo() {
    // Doc-comment in PostOrderContext::reset promises: if `u` precedes `v` in
    // `roots` and there's no path from v to u, then `u` precedes `v` in any RPO.
    // Build two disjoint chains (a -> b, x -> y) and pass roots in [a, x] order.
    let g = common::graph(
        "a -> b
         x -> y",
    );
    let a = g.node("a");
    let x = g.node("x");
    let mut po: Vec<_> = entity_postorder(&g, [a, x]).collect();
    po.reverse(); // RPO
    let names: Vec<_> = po.iter().map(|&n| g.name(n).to_owned()).collect();
    let pos_a = names.iter().position(|s| s == "a").unwrap();
    let pos_x = names.iter().position(|s| s == "x").unwrap();
    assert!(
        pos_a < pos_x,
        "expected a (first root) to precede x in RPO, got {names:?}"
    );
}

#[test]
fn nop_tracker_on_a_tree() {
    use graphwalk::{NopTracker, PostOrder};

    // Tree (no cycles, no joins): a -> {b, c}; b -> d.
    let g = common::graph(
        "a -> b, c
         b -> d",
    );

    let order: Vec<_> = PostOrder::<&Graph, NopTracker>::new(&g, [g.entry()])
        .map(|n| g.name(n).to_owned())
        .collect();
    // Each node is visited exactly once even though NopTracker never records visits;
    // this only holds because the input really is a tree.
    assert_eq!(order.len(), 4);
    let mut sorted = order;
    sorted.sort();
    assert_eq!(sorted, vec!["a", "b", "c", "d"]);
}

#[test]
fn duplicate_root_visited_once() {
    // PostOrderContext::next_event drops a second Pre for an already-visited
    // node.  Passing the same root twice must yield exactly one (Pre, Post)
    // pair, not two — this is what makes idempotent root lists safe.
    let g = common::graph("a -> b");
    let a = g.node("a");
    let mut po = entity_postorder(&g, [a, a]);
    let events: Vec<_> = core::iter::from_fn(|| po.next_event()).collect();
    let pre_a = events
        .iter()
        .filter(|(p, n)| matches!(p, WalkPhase::Pre) && *n == a)
        .count();
    let post_a = events
        .iter()
        .filter(|(p, n)| matches!(p, WalkPhase::Post) && *n == a)
        .count();
    assert_eq!(pre_a, 1, "expected one Pre event for `a`, got {events:?}");
    assert_eq!(post_a, 1, "expected one Post event for `a`, got {events:?}");
}
