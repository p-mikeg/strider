#![allow(clippy::unwrap_used, clippy::expect_used)]

//! Tests for `Region::contains_addr` (covers start/end/interior, pcode-interior,
//! before-start, after-end, and the empty-insns invariant-violation branch).

mod common;
use common::{addr, make_region};

use cfg::test_api::Region;
use cfg::RegionTerminator;

#[test]
fn contains_addr_at_start() {
    let r = make_region(&[(0x1000, 0), (0x1010, 0)]);
    assert!(r.contains_addr(addr(0x1000, 0)));
}

#[test]
fn contains_addr_at_end() {
    let r = make_region(&[(0x1000, 0), (0x1010, 0)]);
    assert!(r.contains_addr(addr(0x1010, 0)));
}

#[test]
fn contains_addr_in_interior() {
    let r = make_region(&[(0x1000, 0), (0x1010, 0)]);
    assert!(r.contains_addr(addr(0x1008, 0)));
}

#[test]
fn contains_addr_pcode_interior() {
    let r = make_region(&[(0x1000, 0), (0x1000, 3)]);
    assert!(r.contains_addr(addr(0x1000, 1)));
}

#[test]
fn contains_addr_before_start_returns_false() {
    let r = make_region(&[(0x1000, 0), (0x1010, 0)]);
    assert!(!r.contains_addr(addr(0x0ff8, 0)));
}

#[test]
fn contains_addr_after_end_returns_false() {
    let r = make_region(&[(0x1000, 0), (0x1010, 0)]);
    assert!(!r.contains_addr(addr(0x1014, 0)));
}

#[test]
fn contains_addr_owns_start_addr_for_empty_region() {
    // Empty regions arise from the single-instruction
    // CondBranch-with-OOB-successor fold; they own exactly their
    // start_addr.  Returning `false` for the start-address query
    // would let the work queue build a duplicate region for the same
    // edge target.
    let r = Region {
        start_addr: addr(0x1000, 0),
        insns: Vec::new(),
        terminator: RegionTerminator::Branch,
    };
    assert!(r.contains_addr(addr(0x1000, 0)));
    // Any other address is still not owned by this region.
    assert!(!r.contains_addr(addr(0x1001, 0)));
    assert!(!r.contains_addr(addr(0x0fff, 0)));
}
