//! Test-only API. Not covered by semver.
//!
//! Re-exports crate internals so integration tests under
//! `crates/cfg/tests/` can exercise every function with logic directly.
//! Not intended for use from downstream crates.

#[doc(hidden)]
pub use crate::cfg::test_api::*;

// Task 4 will add region_builder and dot re-exports here.
