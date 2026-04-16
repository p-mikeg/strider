#![allow(
    clippy::panic,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::unreachable
)]

//! Verifies that an error originating in `ir` and propagated through `opt`
//! into `analyzer` preserves the origin backtrace and accumulates a location
//! entry per `?` boundary.

use std::backtrace::BacktraceStatus;

/// Threads an `ir::Error` through an outer `opt` `?` site.
fn through_opt() -> Result<(), opt::Error> {
    let e: ir::Error = ir::ErrorKind::AssertionFailed("seed".into()).into();
    Err(e)?;
    unreachable!()
}

/// Threads the result of [`through_opt`] through an outer `analyzer` `?` site.
fn through_analyzer() -> Result<(), analyzer::Error> {
    through_opt()?;
    unreachable!()
}

#[test]
fn error_accumulates_locations_across_three_crates() {
    let err = through_analyzer().unwrap_err();

    // Origin (ir) + opt bridge + analyzer bridge = at least three entries.
    let locations = err.locations();
    assert!(
        locations.len() >= 3,
        "expected ≥3 location entries (ir + opt + analyzer), got {}: {:?}",
        locations.len(),
        locations,
    );

    let status = err.backtrace().status();
    assert!(
        !matches!(status, BacktraceStatus::Unsupported),
        "backtrace status should not be Unsupported, got {status:?}",
    );

    // The kind should be the nested ir::AssertionFailed coming out of opt.
    match err.kind() {
        analyzer::ErrorKind::OptError(opt::ErrorKind::IrError(ir::ErrorKind::AssertionFailed(
            msg,
        ))) => {
            assert_eq!(msg, "seed");
        }
        other => panic!("unexpected error kind: {other:?}"),
    }
}
