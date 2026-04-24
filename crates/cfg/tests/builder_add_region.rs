#![allow(clippy::unwrap_used, clippy::expect_used)]

//! Tests for `Builder::add_region` — basic insertion, `EmptyRegion` error,
//! and two-region preservation of indices.

mod common;
use common::{addr, make_builder, make_region};

use cfg::test_api::Region;
use cfg::{test_api, ErrorKind};
use std::collections::VecDeque;

#[test]
fn inserts_into_graph_and_map() {
    let mut b = make_builder(0x1000);
    let r = make_region(&[(0x1000, 0), (0x1004, 0)]);
    let id = test_api::add_region(&mut b, r).unwrap();

    assert!(test_api::graph(&b).node_weight(id).is_some());
    assert_eq!(
        test_api::start_addr_to_region_id(&b).get(&addr(0x1000, 0)),
        Some(&id)
    );
}

#[test]
fn empty_region_returns_error() {
    let mut b = make_builder(0x1000);
    let empty = Region {
        start_addr: addr(0x1000, 0),
        insns: VecDeque::new(),
        ends_with_tail_call: false,
    };
    let err = test_api::add_region(&mut b, empty).unwrap_err();
    assert!(matches!(err.kind(), ErrorKind::EmptyRegion(_)));
}

#[test]
fn two_regions_both_present_with_distinct_indices() {
    let mut b = make_builder(0x1000);
    let r1 = make_region(&[(0x1000, 0)]);
    let r2 = make_region(&[(0x1010, 0)]);
    let id1 = test_api::add_region(&mut b, r1).unwrap();
    let id2 = test_api::add_region(&mut b, r2).unwrap();

    assert_ne!(id1, id2);
    assert_eq!(test_api::graph(&b).node_count(), 2);
    assert_eq!(test_api::start_addr_to_region_id(&b)[&addr(0x1000, 0)], id1);
    assert_eq!(test_api::start_addr_to_region_id(&b)[&addr(0x1010, 0)], id2);
}
