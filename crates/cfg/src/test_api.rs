//! Test-only API. Not covered by semver.
//!
//! Re-exports crate internals so integration tests under
//! `crates/cfg/tests/` can exercise every function with logic directly.
//! Not intended for use from downstream crates.

// Per-module sub-modules are added in the tasks that need them
// (Task 3: builder, Task 4: region_builder + dot).
