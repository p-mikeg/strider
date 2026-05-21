#![cfg_attr(
    test,
    allow(
        clippy::panic,
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::unreachable
    )
)]

//! Strider lift: target descriptions, sleigh integration, CFG construction,
//! and IR lifting.  Consolidation crate for what previously lived in
//! standalone `target`, `pcode-lift`, and `cfg` crates.

pub mod pcode_lift;
pub mod cfg;
pub mod lifter;
pub mod region_driver;

/// Test-only re-exports for the `cfg` module, mirroring the flat
/// `test_api` surface the standalone `cfg` crate used to expose so
/// integration tests under `crates/cfg/tests/` keep compiling.
#[doc(hidden)]
pub mod cfg_test_api;
