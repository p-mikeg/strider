//! Shared fixture builders for the tier-2 classifier integration tests.
//!
//! W7 split: previously a single 879-line `tier2_helpers.rs`, now a
//! directory module with focused sub-files:
//!
//! | Sub-module        | Contents                                                  |
//! |-------------------|-----------------------------------------------------------|
//! | [`orchestrator`]  | End-to-end pipeline runners + placeholder-anchor finders. |
//! | [`classify`]      | One fixture per producer-shape arm of `classify_anchor`.  |
//! | [`inplace`]       | In-place-edit-test fixtures (today: re-exports).          |
//! | [`cache`]         | Cache-contract-test fixtures (today: re-exports).         |
//!
//! Every public helper is re-exported flat from this `mod.rs` so callers
//! continue to write `use common::tier2_helpers::build_X;` exactly as they
//! did pre-W7.  The flat re-export surface is the **stable public API**;
//! tests should not import from sub-modules directly.
//!
//! IMPORTANT: the anchor returned is **NOT** the original `NodeOutputId`
//! recorded at lift time — that id can be invalidated by `ConstantFold`'s
//! `replace_all_uses` rewires.  Instead, helpers resolve the placeholder
//! Return's current value-input (slot 2) on the post-optimisation graph
//! and return that.  This mirrors what the R3 orchestrator does at each
//! tier-2 invocation: walk to the Return's input slot to find the live
//! producer.
//!
//! The fixtures intentionally use small hand-assembled byte sequences so
//! the failure modes are attributable to the classifier under test (or to
//! the optimiser passes the helper runs), not to a build pipeline whose
//! contents the caller has to reason about.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, dead_code)]

pub mod cache;
pub mod classify;
pub mod inplace;
pub mod orchestrator;

// ── Flat re-export surface (preserved across the W7 split) ──────────────────
//
// Each integration test (`tests/<x>.rs`) compiles `common::tier2_helpers`
// independently and uses only a subset of these helpers, so per-test the
// other re-exports look "unused".  `#[allow(unused_imports)]` silences the
// per-test compile noise without weakening the rest of the test suite's
// dead-code hygiene (the underlying definitions in the `classify` /
// `orchestrator` sub-modules are still subject to dead-code analysis).

#[allow(unused_imports)]
pub use classify::{
    build_bx_lr_scenario, build_initial_var_target_scenario_x86_64,
    build_int_const_target_scenario, build_int_const_target_scenario_via_stack,
    build_jump_table_known_bits_scenario, build_jump_table_predecessor_if_scenario,
    build_jump_table_unbounded_scenario, build_non_jump_table_load_scenario,
    build_pop_pc_via_stack_load_forward_scenario, build_push_target_pop_pc_scenario,
    build_value_phi_target_scenario,
};
#[allow(unused_imports)]
pub use orchestrator::{anchor_value_input, run_pipeline_x86_64};
