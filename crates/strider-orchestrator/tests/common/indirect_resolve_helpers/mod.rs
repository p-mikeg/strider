//! Shared fixture builders for the IR-level classifier integration tests.
//!
//! Two sub-modules:
//!
//! | Sub-module        | Contents                                                  |
//! |-------------------|-----------------------------------------------------------|
//! | [`orchestrator`]  | End-to-end pipeline runners + placeholder-target finders. |
//! | [`classify`]      | One fixture per producer-shape arm of `classify_target`.  |
//!
//! The flat re-export surface in this `mod.rs` is the stable public
//! API; tests should not import from sub-modules directly.
//!
//! IMPORTANT: the target returned is **NOT** the original
//! `ValueId` recorded at lift time; that id can be invalidated
//! by `ConstantFold`'s `replace_all_uses` rewires.  Instead, helpers
//! resolve the placeholder IndirectBranch's current value-input on the
//! post-optimisation graph and return that.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, dead_code)]

pub(crate) mod classify;
pub(crate) mod orchestrator;

// Each integration test compiles `common::indirect_resolve_helpers` independently
// and uses only a subset of these helpers, so per-test the unused
// re-exports look "unused".  `#[allow(unused_imports)]` silences the
// per-test compile noise; the underlying definitions in the
// `classify` / `orchestrator` sub-modules remain subject to
// dead-code analysis.
#[allow(unused_imports)]
pub(crate) use classify::{
    build_bx_lr_scenario, build_initial_var_target_scenario_x86_64,
    build_int_const_target_scenario_via_stack, build_jump_table_known_bits_scenario,
    build_jump_table_predecessor_if_scenario, build_jump_table_unbounded_scenario,
    build_non_jump_table_load_scenario, build_pop_pc_via_stack_load_forward_scenario,
    build_push_target_pop_pc_scenario, build_stack_array_dispatch_scenario,
};
#[allow(unused_imports)]
pub(crate) use orchestrator::{run_pipeline_x86_64, target_value_input};
