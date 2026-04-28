//! Fixture builders specific to the **in-place edit** tests
//! (`tests/tier2_in_place_edits.rs`).
//!
//! Currently these tests only reuse the `build_initial_var_target_scenario_x86_64`
//! fixture from `super::classify` — the in-place edit machinery exercises
//! the orchestrator's tail-call / link-register dispatch on top of an
//! already-classified anchor, so no new fixture builder is required.  The
//! sub-module exists per W7 so future inplace-only fixtures land in a
//! focused file rather than re-bloating `tier2_helpers.rs`.

#![allow(unused_imports, dead_code)]

pub use super::classify::build_initial_var_target_scenario_x86_64;
