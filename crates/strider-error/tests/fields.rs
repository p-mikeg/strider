#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used)]

//! Pins the public contract of `ErrorFields::new` and `push_caller`.
//! These invariants are assumed by every `define_error!` wrapper and by
//! `dot::error::Error<E>`; this file is the one place they're tested directly.

use std::backtrace::BacktraceStatus;
use strider_error::ErrorFields;

#[test]
fn new_seeds_single_location_and_valid_backtrace_status() {
    let f = ErrorFields::new();
    assert_eq!(f.locations.len(), 1, "chain should have exactly one entry at construction");
    // Status is env-dependent — we accept any of the three documented values.
    let s = f.backtrace.status();
    assert!(
        matches!(s, BacktraceStatus::Captured | BacktraceStatus::Disabled | BacktraceStatus::Unsupported),
        "unexpected backtrace status: {s:?}",
    );
}

#[test]
fn push_caller_appends_location_without_touching_backtrace() {
    let f = ErrorFields::new();
    let before_ptr: *const _ = &*f.backtrace;
    let f = f.push_caller();
    assert_eq!(f.locations.len(), 2, "chain should grow by one per push_caller");
    let after_ptr: *const _ = &*f.backtrace;
    assert_eq!(
        before_ptr, after_ptr,
        "push_caller must not reallocate the backtrace",
    );
}

#[test]
fn repeated_push_caller_grows_chain_linearly() {
    let f = (0..5).fold(ErrorFields::new(), |acc, _| acc.push_caller());
    assert_eq!(f.locations.len(), 6, "1 from new() + 5 from push_caller()");
}
