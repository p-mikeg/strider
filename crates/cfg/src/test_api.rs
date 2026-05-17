//! Test-only API. Not covered by semver.
//!
//! Re-exports crate internals so integration tests under
//! `crates/cfg/tests/` can exercise every function with logic directly.
//! Not intended for use from downstream crates.

#[doc(hidden)]
pub use crate::cfg::test_api::*;

#[doc(hidden)]
pub use crate::cfg::region_builder_test_api::{
    ProcessInsnRes, TestRegionBuilder, next_pcode_addr,
};

#[doc(hidden)]
pub use crate::cfg::indirect_resolve_test_api::{
    ResolvedTargets, build_resolver_mini_graph_for_test, resolve_indirect_target_for_test,
};

#[doc(hidden)]
pub use crate::cfg::dot_test_api::vn_to_name;
