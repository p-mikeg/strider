//! Fixture builders specific to the **cache contract** tests
//! (`tests/tier2_cache.rs`, `tier2_optimizer_tiers.rs`).
//!
//! Currently these tests only reuse `build_initial_var_target_scenario_x86_64`
//! from `super::classify` — the cache + pipeline-tier tests assert behaviour
//! across the lift→optimise→re-lift cycle on top of an already-classified
//! anchor, so no new fixture builder is required.  The sub-module exists
//! per W7 so future cache-only fixtures land in a focused file rather than
//! re-bloating `tier2_helpers.rs`.

#![allow(unused_imports, dead_code)]

pub use super::classify::build_initial_var_target_scenario_x86_64;
