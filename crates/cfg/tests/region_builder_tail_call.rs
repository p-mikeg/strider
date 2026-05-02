#![allow(clippy::unwrap_used, clippy::expect_used)]

//! Tests for `RegionBuilder::is_branch_tail_call_nocheck` and
//! `RegionBuilder::is_branch_tail_call`.

mod common;
use common::{addr, make_builder, make_builder_opts, make_region_builder};

use cfg::OptionsBuilder;

#[test]
fn nocheck_below_start_default_opts_is_tail_call() {
    let mut b = make_builder(0x1000);
    let rb = make_region_builder(&mut b, addr(0x1000, 0));
    assert!(rb.is_branch_tail_call_nocheck(addr(0x0800, 0)));
}

#[test]
fn nocheck_below_start_with_allow_is_not_tail_call() {
    let opts = OptionsBuilder::new().allow_code_before_start_addr().build();
    let mut b = make_builder_opts(0x1000, opts);
    let rb = make_region_builder(&mut b, addr(0x1000, 0));
    assert!(!rb.is_branch_tail_call_nocheck(addr(0x0800, 0)));
}

/// When `fn_max_size` is set the function's extent is known precisely
/// as `[start, start + max)`.  Any backward jump below `start` then
/// goes to a *different* function and must be classified as a tail
/// call, regardless of the legacy `allow_code_before_start_addr`
/// flag — that flag only allows reach-back into prelude/unwind in the
/// *unbounded* case where we don't know the function's extent.
#[test]
fn nocheck_below_start_with_allow_and_fn_max_size_is_tail_call() {
    let opts = OptionsBuilder::new()
        .allow_code_before_start_addr()
        .set_function_max_size(0x100)
        .build();
    let mut b = make_builder_opts(0x1000, opts);
    let rb = make_region_builder(&mut b, addr(0x1000, 0));
    assert!(
        rb.is_branch_tail_call_nocheck(addr(0x0800, 0)),
        "with fn_max_size set, backward jumps below start must be tail calls regardless of allow_code_before_start_addr"
    );
}

/// Companion: `fn_max_size` set, `allow_code_before_start_addr` NOT
/// set, backward target below `start`.  Must still classify as a tail
/// call (the strict-lower-bound branch).
#[test]
fn nocheck_below_start_with_fn_max_size_no_allow_is_tail_call() {
    let opts = OptionsBuilder::new().set_function_max_size(0x100).build();
    let mut b = make_builder_opts(0x1000, opts);
    let rb = make_region_builder(&mut b, addr(0x1000, 0));
    assert!(rb.is_branch_tail_call_nocheck(addr(0x0800, 0)));
}

#[test]
fn nocheck_within_function_no_limit_is_not_tail_call() {
    let mut b = make_builder(0x1000);
    let rb = make_region_builder(&mut b, addr(0x1000, 0));
    assert!(!rb.is_branch_tail_call_nocheck(addr(0x1200, 0)));
}

#[test]
fn nocheck_at_fn_max_size_boundary() {
    let opts = OptionsBuilder::new().set_function_max_size(0x100).build();
    let mut b = make_builder_opts(0x1000, opts);
    let rb = make_region_builder(&mut b, addr(0x1000, 0));
    // Contract: target at exactly start + max_size is a tail call (inclusive boundary).
    assert!(rb.is_branch_tail_call_nocheck(addr(0x1100, 0)));
    assert!(!rb.is_branch_tail_call_nocheck(addr(0x10ff, 0)));
}

#[test]
fn check_valid_insn_index_zero_is_tail_call() {
    let mut b = make_builder(0x1000);
    let rb = make_region_builder(&mut b, addr(0x1000, 0));
    assert!(matches!(rb.is_branch_tail_call(addr(0x0800, 0)), Ok(true)));
}

#[test]
fn check_invalid_insn_index_nonzero_returns_error() {
    let mut b = make_builder(0x1000);
    let rb = make_region_builder(&mut b, addr(0x1000, 0));
    let err = rb.is_branch_tail_call(addr(0x0800, 3)).unwrap_err();
    assert!(err.to_string().contains("invalid tail call"), "got: {err}");
}

#[test]
fn check_inside_function_any_insn_index_is_not_tail_call() {
    let mut b = make_builder(0x1000);
    let rb = make_region_builder(&mut b, addr(0x1000, 0));
    assert!(matches!(rb.is_branch_tail_call(addr(0x1200, 7)), Ok(false)));
}

/// Pinned contract: when `start_addr + fn_max_size` would overflow u64,
/// the tail-call bound check must not silently wrap. Current code computes
/// `fn_max_size + start_addr.addr <= addr.addr` with unchecked `+`, so an
/// overflow produces a tiny wrapped number that trivially <= addr, flipping
/// every branch to "tail call". Fix: saturate on overflow.
#[test]
fn fn_max_size_plus_start_addr_overflow_treats_inside_range_as_non_tail_call() {
    use cfg::test_api::Options;

    // start_addr near top of u64; fn_max_size large enough that the raw sum
    // would overflow but every plausible target still lies inside the function.
    let start_addr = u64::MAX - 0x100;
    let max_size = 0x1000u64; // start + max overflows
    let opts: Options = cfg::OptionsBuilder::new()
        .set_function_max_size(max_size)
        .build();

    let mut b = common::make_builder_opts(start_addr, opts);
    let rb = common::make_region_builder(&mut b, common::addr(start_addr, 0));

    // Target inside [start, start+max) (but the raw sum would overflow).
    let target = common::addr(start_addr + 0x10, 0);
    assert!(
        !rb.is_branch_tail_call_nocheck(target),
        "target inside function range must NOT classify as tail call even when start+max overflows"
    );
}
