#![allow(clippy::unwrap_used, clippy::expect_used)]

//! Tests for `Builder::find_region_containing_addr` — full grid of positive
//! and negative cases.

mod common;
use common::{addr, make_builder, make_region};

use cfg::test_api;

#[test]
fn empty_graph_returns_none() {
    let b = make_builder(0x1000);
    assert!(test_api::find_region_containing_addr(&b, addr(0x1000, 0)).is_none());
}

#[test]
fn at_start_addr() {
    let mut b = make_builder(0x1000);
    let id = test_api::add_region(&mut b, make_region(&[(0x1000, 0), (0x100f, 0)])).unwrap();
    assert_eq!(
        test_api::find_region_containing_addr(&b, addr(0x1000, 0)).map(|(i, _)| i),
        Some(id)
    );
}

#[test]
fn at_interior_addr() {
    let mut b = make_builder(0x1000);
    let id = test_api::add_region(&mut b, make_region(&[(0x1000, 0), (0x100f, 0)])).unwrap();
    assert_eq!(
        test_api::find_region_containing_addr(&b, addr(0x1008, 0)).map(|(i, _)| i),
        Some(id)
    );
}

#[test]
fn at_last_insn() {
    let mut b = make_builder(0x1000);
    let id = test_api::add_region(&mut b, make_region(&[(0x1000, 0), (0x100f, 0)])).unwrap();
    assert_eq!(
        test_api::find_region_containing_addr(&b, addr(0x100f, 0)).map(|(i, _)| i),
        Some(id)
    );
}

#[test]
fn beyond_end_returns_none() {
    let mut b = make_builder(0x1000);
    test_api::add_region(&mut b, make_region(&[(0x1000, 0), (0x100f, 0)])).unwrap();
    assert!(test_api::find_region_containing_addr(&b, addr(0x1020, 0)).is_none());
}

#[test]
fn two_adjacent_regions_route_correctly() {
    let mut b = make_builder(0x1000);
    let id1 = test_api::add_region(&mut b, make_region(&[(0x1000, 0), (0x100f, 0)])).unwrap();
    let id2 = test_api::add_region(&mut b, make_region(&[(0x1010, 0), (0x1020, 0)])).unwrap();

    assert_eq!(
        test_api::find_region_containing_addr(&b, addr(0x1004, 0)).map(|(i, _)| i),
        Some(id1)
    );
    assert_eq!(
        test_api::find_region_containing_addr(&b, addr(0x1010, 0)).map(|(i, _)| i),
        Some(id2)
    );
    assert_eq!(
        test_api::find_region_containing_addr(&b, addr(0x1018, 0)).map(|(i, _)| i),
        Some(id2)
    );
}
