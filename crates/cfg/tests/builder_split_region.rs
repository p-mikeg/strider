#![allow(clippy::unwrap_used, clippy::expect_used)]

//! Tests for `Builder::split_region` — no-op at start, basic split, address
//! ranges after split, Fallthrough edge insertion, incoming-edge rewiring,
//! and the `FailedSplitingRegion` error.

mod common;
use common::{addr, make_builder, make_region};

use cfg::{test_api, ErrorKind, RegionEdgeKind};
use petgraph::visit::{EdgeRef, IntoEdgeReferences};

#[test]
fn split_at_start_is_noop() {
    let mut b = make_builder(0x1000);
    let id = test_api::add_region(
        &mut b,
        make_region(&[(0x1000, 0), (0x1004, 0), (0x1008, 0)]),
    ).unwrap();
    let result = test_api::split_region(&mut b, id, addr(0x1000, 0)).unwrap();

    assert_eq!(result, id, "split at start must return original id");
    assert_eq!(test_api::graph(&b).node_count(), 1, "no new region created");
}

#[test]
fn split_creates_two_regions_second_keeps_original_id() {
    let mut b = make_builder(0x1000);
    let original = test_api::add_region(
        &mut b,
        make_region(&[(0x1000, 0), (0x1004, 0), (0x1008, 0), (0x100c, 0)]),
    ).unwrap();
    let second = test_api::split_region(&mut b, original, addr(0x1008, 0)).unwrap();

    // Contract: the second half keeps the original NodeIndex so outgoing
    // edges and work-queue parent references remain valid.
    assert_eq!(second, original);
    assert_eq!(test_api::graph(&b).node_count(), 2);
}

#[test]
fn split_produces_correct_addr_ranges() {
    let mut b = make_builder(0x1000);
    let original = test_api::add_region(
        &mut b,
        make_region(&[(0x1000, 0), (0x1004, 0), (0x1008, 0), (0x100c, 0)]),
    ).unwrap();
    test_api::split_region(&mut b, original, addr(0x1008, 0)).unwrap();

    assert_eq!(test_api::graph(&b)[original].start_addr, addr(0x1008, 0));
    assert_eq!(test_api::graph(&b)[original].insns.len(), 2);

    let first_id = test_api::start_addr_to_region_id(&b)[&addr(0x1000, 0)];
    assert_eq!(test_api::graph(&b)[first_id].start_addr, addr(0x1000, 0));
    assert_eq!(test_api::graph(&b)[first_id].insns.len(), 2);
}

#[test]
fn split_adds_fallthrough_edge() {
    let mut b = make_builder(0x1000);
    let original = test_api::add_region(
        &mut b,
        make_region(&[(0x1000, 0), (0x1004, 0), (0x1008, 0)]),
    ).unwrap();
    test_api::split_region(&mut b, original, addr(0x1008, 0)).unwrap();

    let edges: Vec<_> = test_api::graph(&b).edge_references().collect();
    assert_eq!(edges.len(), 1, "exactly one edge after split");
    assert_eq!(*edges[0].weight(), RegionEdgeKind::Fallthrough);
    assert_eq!(edges[0].target(), original);
}

#[test]
fn split_rewires_incoming_edges_to_first_half() {
    let mut b = make_builder(0x1000);
    let a = test_api::add_region(&mut b, make_region(&[(0x0ff0, 0)])).unwrap();
    let b_id = test_api::add_region(
        &mut b,
        make_region(&[(0x1000, 0), (0x1004, 0), (0x1008, 0)]),
    ).unwrap();
    test_api::graph_mut(&mut b).add_edge(a, b_id, RegionEdgeKind::Branch);

    test_api::split_region(&mut b, b_id, addr(0x1004, 0)).unwrap();

    let first = test_api::start_addr_to_region_id(&b)[&addr(0x1000, 0)];
    let incoming: Vec<_> = test_api::graph(&b)
        .edges_directed(first, petgraph::Incoming)
        .collect();
    assert_eq!(incoming.len(), 1);
    assert_eq!(*incoming[0].weight(), RegionEdgeKind::Branch);
    assert_eq!(incoming[0].source(), a);

    let second_branch_incoming: Vec<_> = test_api::graph(&b)
        .edges_directed(b_id, petgraph::Incoming)
        .filter(|e| *e.weight() == RegionEdgeKind::Branch)
        .collect();
    assert!(second_branch_incoming.is_empty());
}

#[test]
fn split_addr_not_in_region_insns_returns_failed_splitting_region() {
    // Region has insns at 0x1000 and 0x1010 only — nothing at 0x1008.
    // split_region expects `addr` to match an exact insn addr.
    let mut b = make_builder(0x1000);
    let id = test_api::add_region(&mut b, make_region(&[(0x1000, 0), (0x1010, 0)])).unwrap();
    let err = test_api::split_region(&mut b, id, addr(0x1008, 0)).unwrap_err();
    assert!(matches!(
        err.kind(),
        ErrorKind::FailedSplitingRegion(_, a) if *a == addr(0x1008, 0)
    ));
}
