//! Fixture builders for the IR-level classifier tests: [`orchestrator`] holds
//! the pipeline runners and placeholder-target finders, [`classify`] one
//! fixture per producer-shape arm of `classify_target`.  Import through the
//! flat re-exports below, not from the sub-modules.
//!
//! The target these helpers return is NOT the `ValueId` recorded at lift time:
//! `ConstantFold`'s `replace_value` rewires can invalidate that id, so they
//! re-read the placeholder `IndirectBranch`'s current value-input off the
//! post-optimisation graph.

#![allow(dead_code)]

pub(crate) mod classify;
pub(crate) mod orchestrator;

// Each integration test uses a subset of these helpers, so the re-exports are
// `allow(unused_imports)`.
#[allow(unused_imports)]
pub(crate) use classify::{
    build_bx_lr_scenario, build_initial_var_target_scenario_x86_64,
    build_int_const_target_scenario_via_stack, build_jump_table_known_bits_scenario,
    build_jump_table_predecessor_if_scenario, build_jump_table_unbounded_scenario,
    build_non_jump_table_load_scenario, build_pop_pc_via_stack_load_forward_scenario,
    build_push_target_pop_pc_scenario, build_stack_array_dispatch_scenario,
};
#[allow(unused_imports)]
pub(crate) use orchestrator::target_value_input;
