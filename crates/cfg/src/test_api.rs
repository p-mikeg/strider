//! Test-only API. Not covered by semver.
//!
//! Re-exports crate internals so integration tests under
//! `crates/cfg/tests/` can exercise every function with logic directly.
//! Not intended for use from downstream crates.

#[doc(hidden)]
pub use crate::cfg::test_api::*;

#[doc(hidden)]
pub use crate::cfg::region_builder_test_api::{ProcessInsnRes, TestRegionBuilder};

#[doc(hidden)]
pub use crate::cfg::dot_test_api::vn_to_name;
