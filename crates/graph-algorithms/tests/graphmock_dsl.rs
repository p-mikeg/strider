#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::graph;
use graph_algorithms::walk::GraphRef;
use std::ops::ControlFlow;

#[test]
fn parses_linear_chain() {
    let _ = graph(
        "
        a -> b
        b -> c
        c -> d
    ",
    );
}

#[test]
fn parses_diamond_topology() {
    let _ = graph(
        "
        a -> b, c
        b, c -> d
    ",
    );
}

#[test]
fn parses_self_referential_cycle() {
    let _ = graph(
        "
        a -> b
        b -> c
        c -> b
        c -> d
    ",
    );
}

fn succs(g: &common::Graph, node: common::NodeId) -> Vec<String> {
    let mut out = Vec::new();
    let _ = g.try_successors(node, |s| {
        out.push(g.name(s).to_owned());
        ControlFlow::Continue(())
    });
    out
}

#[test]
#[allow(clippy::many_single_char_names)]
fn fan_out_and_fan_in() {
    let g = graph("a, b -> c, d");
    let a = g.node("a");
    let b = g.node("b");
    assert_eq!(succs(&g, a), vec!["c", "d"]);
    assert_eq!(succs(&g, b), vec!["c", "d"]);
}

#[test]
fn self_loop() {
    let g = graph("a -> a");
    let a = g.node("a");
    assert_eq!(succs(&g, a), vec!["a"]);
}

#[test]
fn name_recurrence_resolves_to_same_id() {
    let g = graph(
        "a -> b
         b -> a",
    );
    let a1 = g.node("a");
    let a2 = g.node("a");
    assert_eq!(a1, a2);
    assert_eq!(succs(&g, a1), vec!["b"]);
}

#[test]
#[should_panic(expected = "graphmock: empty node name")]
fn empty_succ_token_panics() {
    let _ = graph("a -> ");
}

#[test]
#[should_panic(expected = "graphmock: empty node name")]
fn empty_pred_token_panics() {
    let _ = graph(" -> b");
}

#[test]
#[should_panic(expected = "graphmock: empty node name")]
fn trailing_comma_panics() {
    let _ = graph("a, -> b");
}
