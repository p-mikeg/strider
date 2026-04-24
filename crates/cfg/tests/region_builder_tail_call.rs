#![allow(clippy::unwrap_used, clippy::expect_used)]

//! Tests for `RegionBuilder::is_branch_tail_call_nocheck` and
//! `RegionBuilder::is_branch_tail_call`.

mod common;
use common::{addr, make_builder, make_builder_opts, make_region_builder};

use cfg::{ErrorKind, OptionsBuilder};

#[test]
fn nocheck_below_start_default_opts_is_tail_call() {
    let mut b = make_builder(0x1000);
    let mut rb = make_region_builder(&mut b, addr(0x1000, 0));
    assert!(rb.is_branch_tail_call_nocheck(addr(0x0800, 0)));
}

#[test]
fn nocheck_below_start_with_allow_is_not_tail_call() {
    let opts = OptionsBuilder::new().allow_code_before_start_addr().build();
    let mut b = make_builder_opts(0x1000, opts);
    let mut rb = make_region_builder(&mut b, addr(0x1000, 0));
    assert!(!rb.is_branch_tail_call_nocheck(addr(0x0800, 0)));
}

#[test]
fn nocheck_within_function_no_limit_is_not_tail_call() {
    let mut b = make_builder(0x1000);
    let mut rb = make_region_builder(&mut b, addr(0x1000, 0));
    assert!(!rb.is_branch_tail_call_nocheck(addr(0x1200, 0)));
}

#[test]
fn nocheck_at_fn_max_size_boundary() {
    let opts = OptionsBuilder::new().set_function_max_size(0x100).build();
    let mut b = make_builder_opts(0x1000, opts);
    let mut rb = make_region_builder(&mut b, addr(0x1000, 0));
    // Contract: target at exactly start + max_size is a tail call (inclusive boundary).
    assert!(rb.is_branch_tail_call_nocheck(addr(0x1100, 0)));
    assert!(!rb.is_branch_tail_call_nocheck(addr(0x10ff, 0)));
}

#[test]
fn check_valid_insn_index_zero_is_tail_call() {
    let mut b = make_builder(0x1000);
    let mut rb = make_region_builder(&mut b, addr(0x1000, 0));
    assert!(matches!(rb.is_branch_tail_call(addr(0x0800, 0)), Ok(true)));
}

#[test]
fn check_invalid_insn_index_nonzero_returns_error() {
    let mut b = make_builder(0x1000);
    let mut rb = make_region_builder(&mut b, addr(0x1000, 0));
    let err = rb.is_branch_tail_call(addr(0x0800, 3)).unwrap_err();
    assert!(matches!(err.kind(), ErrorKind::InvalidTailCall(_)));
}

#[test]
fn check_inside_function_any_insn_index_is_not_tail_call() {
    let mut b = make_builder(0x1000);
    let mut rb = make_region_builder(&mut b, addr(0x1000, 0));
    assert!(matches!(rb.is_branch_tail_call(addr(0x1200, 7)), Ok(false)));
}
