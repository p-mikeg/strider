//! An `If` arm's consumer is a second pattern joined on the captured edge:
//! `capture_true(t)` on the `If`, `t` on the consumer's control input.

use strider_ir::IRViewer;
use strider_ir::node::{NodeId, NodeKind};
use strider_pattern::{Capture, JoinConstraint, Matcher, Pattern, if_else, region, var};

use super::support::shapes;

/// The single consumer of the `If`'s control output `slot`.
fn arm_consumer(f: &strider_ir::Function, slot: usize) -> NodeId {
    let iff = f
        .graph()
        .all_node_ids()
        .find(|&n| matches!(f.node_kind(n), NodeKind::If))
        .expect("an If");
    let out = f.node_outputs(iff)[slot];
    let mut uses = f.value_uses(out);
    let (consumer, _) = uses.next().expect("a consumer");
    assert!(uses.next().is_none(), "one consumer");
    consumer
}

/// A `Region` fed by the edge `edge`, binding `r`.
fn region_on(edge: Capture, r: Capture) -> Pattern {
    region().capture(r).any_input(var(edge)).build()
}

#[test]
fn the_true_edge_joins_its_consumer() {
    let f = shapes::if_cmp_then_return(4);
    let m = Matcher::new(&f);
    let (t, r) = (Capture::new(), Capture::new());

    let rows = m
        .find_joined_constrained(&[&if_else().capture_true(t).build(), &region_on(t, r)], &[])
        .unwrap();
    let joined: Vec<NodeId> = rows
        .iter()
        .filter_map(|row| row[1].node(r, f.graph()))
        .collect();
    assert_eq!(joined, vec![arm_consumer(&f, 0)]);
}

#[test]
fn both_edges_join_their_own_consumers() {
    let f = shapes::if_cmp_then_return(4);
    let m = Matcher::new(&f);
    let (t, e, rt, re) = (
        Capture::new(),
        Capture::new(),
        Capture::new(),
        Capture::new(),
    );

    let rows = m
        .find_joined_constrained(
            &[
                &if_else().capture_true(t).capture_false(e).build(),
                &region_on(t, rt),
                &region_on(e, re),
            ],
            &[],
        )
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][1].node(rt, f.graph()), Some(arm_consumer(&f, 0)));
    assert_eq!(rows[0][2].node(re, f.graph()), Some(arm_consumer(&f, 1)));
}

/// The consumer is never the `If`'s condition, so one capture on both cannot
/// agree.
#[test]
fn a_capture_shared_by_the_cond_and_the_consumer_must_agree() {
    let f = shapes::if_cmp_then_return(4);
    let m = Matcher::new(&f);
    let t = Capture::new();

    let shared = Capture::new();
    let rows = m
        .find_joined_constrained(
            &[
                &if_else().cond(var(shared)).capture_true(t).build(),
                &region_on(t, shared),
            ],
            &[],
        )
        .unwrap();
    assert!(rows.is_empty());

    let (a, b) = (Capture::new(), Capture::new());
    let rows = m
        .find_joined_constrained(
            &[
                &if_else().cond(var(a)).capture_true(t).build(),
                &region_on(t, b),
            ],
            &[],
        )
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][1].node(b, f.graph()), Some(arm_consumer(&f, 0)));
}

/// The two arms have distinct consumers, so one capture cannot cover both.
#[test]
fn a_capture_shared_by_both_consumers_must_agree() {
    let f = shapes::if_cmp_then_return(4);
    let m = Matcher::new(&f);
    let (t, e) = (Capture::new(), Capture::new());
    let iff = || if_else().capture_true(t).capture_false(e).build();

    let shared = Capture::new();
    let rows = m
        .find_joined_constrained(&[&iff(), &region_on(t, shared), &region_on(e, shared)], &[])
        .unwrap();
    assert!(rows.is_empty());

    let (rt, re) = (Capture::new(), Capture::new());
    let rows = m
        .find_joined_constrained(&[&iff(), &region_on(t, rt), &region_on(e, re)], &[])
        .unwrap();
    assert_eq!(rows.len(), 1);
}

/// A constraint may relate the `If` to its arm's consumer.
#[test]
fn a_constraint_may_mention_the_consumer() {
    let f = shapes::if_cmp_then_return(4);
    let m = Matcher::new(&f);
    let (if_cap, t, r) = (Capture::new(), Capture::new(), Capture::new());

    let rows = m
        .find_joined_constrained(
            &[
                &if_else().capture(if_cap).capture_true(t).build(),
                &region_on(t, r),
            ],
            &[JoinConstraint::Dominates {
                dominator: if_cap,
                dominated: r,
            }],
        )
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][1].node(r, f.graph()), Some(arm_consumer(&f, 0)));
}
