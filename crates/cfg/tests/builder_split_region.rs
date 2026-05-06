#![allow(clippy::unwrap_used, clippy::expect_used)]

//! Tests for `Builder::split_region` — no-op at start, basic split, address
//! ranges after split, Fallthrough edge insertion, incoming-edge rewiring,
//! and the `FailedSplitingRegion` error.

mod common;
use common::{addr, make_builder, make_region};

use cfg::{test_api, RegionEdgeKind};
use petgraph::visit::{EdgeRef, IntoEdgeReferences};

#[test]
fn split_at_start_is_noop() {
    let mut b = make_builder(0x1000);
    let id = test_api::add_region(
        &mut b,
        make_region(&[(0x1000, 0), (0x1004, 0), (0x1008, 0)]),
    ).unwrap();

    let edges_before = test_api::graph(&b).edge_references().count();
    let map_len_before = test_api::start_addr_to_region_id(&b).len();

    let result = test_api::split_region(&mut b, id, addr(0x1000, 0)).unwrap();

    assert_eq!(result, id, "split at start must return original id");
    assert_eq!(test_api::graph(&b).node_count(), 1, "no new region created");
    assert_eq!(
        test_api::graph(&b).edge_references().count(),
        edges_before,
        "no-op split must not add an edge",
    );
    assert_eq!(
        test_api::start_addr_to_region_id(&b).len(),
        map_len_before,
        "no-op split must not insert into start_addr_to_region_id",
    );
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

/// Bug fix regression: when `split_addr` falls *between* two recorded
/// insns — the case AArch64 PAC instructions (`paciasp`, `autiasp`)
/// create when they lift to zero pcode ops, leaving a hole in the
/// per-region address range — the split must round down to the
/// largest insn whose address is ≤ `split_addr`.  The second region
/// keeps the requested `split_addr` as its `start_addr` so future
/// lookups for that exact address resolve correctly.
#[test]
fn split_addr_in_zero_pcode_hole_rounds_down_to_largest_le() {
    // Region [(0x1000), (0x1004), (0x100c)] — note the hole between
    // 0x1004 and 0x100c that 0x1008 falls into.
    let mut b = make_builder(0x1000);
    let original = test_api::add_region(
        &mut b,
        make_region(&[(0x1000, 0), (0x1004, 0), (0x100c, 0)]),
    )
    .unwrap();

    let second = test_api::split_region(&mut b, original, addr(0x1008, 0)).unwrap();

    // Second half retains original NodeIndex.
    assert_eq!(second, original);
    // Second half's start_addr is the requested split addr (not the
    // first insn's addr), so future lookups for 0x1008 resolve to it.
    assert_eq!(test_api::graph(&b)[original].start_addr, addr(0x1008, 0));
    assert_eq!(test_api::graph(&b)[original].insns.len(), 1);
    assert_eq!(
        test_api::graph(&b)[original].insns[0].addr,
        addr(0x100c, 0),
    );

    // First half holds insns up to and including the rounded-down
    // boundary (0x1004).
    let first_id = test_api::start_addr_to_region_id(&b)[&addr(0x1000, 0)];
    assert_eq!(test_api::graph(&b)[first_id].insns.len(), 2);
    assert_eq!(
        test_api::graph(&b)[first_id].insns.last().unwrap().addr,
        addr(0x1004, 0),
    );

    // start_addr_to_region_id maps the requested split addr to the
    // second half.
    let map = test_api::start_addr_to_region_id(&b);
    assert_eq!(map[&addr(0x1008, 0)], original);
}

/// Defensive: `split_addr` strictly less than every recorded insn is
/// unreachable from the cfg builder's normal call path
/// (`contains_addr` would have returned false), but the `split_region`
/// API is exposed publicly via `test_api` — keep it surfacing an
/// error rather than panicking on the empty `rposition`.  The typed
/// `cfg::SplitAddressNotFoundError` downcast is added separately by
/// the typed-errors fix (see `tests/typed_errors.rs`).
#[test]
fn split_addr_below_every_insn_returns_error() {
    let mut b = make_builder(0x1000);
    let id = test_api::add_region(&mut b, make_region(&[(0x1000, 0), (0x1010, 0)])).unwrap();
    let err = test_api::split_region(&mut b, id, addr(0x0ff0, 0)).unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("not found"), "got: {msg}");
}
