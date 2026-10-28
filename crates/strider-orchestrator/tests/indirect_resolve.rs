//! The indirect-branch resolve loop and its reports.
//!
//! One binary over `indirect_resolve/`, so the shared test tree compiles
//! once for the group instead of once per file.

#[macro_use]
mod common;

#[path = "indirect_resolve/indirect_branch.rs"]
mod indirect_branch;

#[path = "indirect_resolve/indirect_branch_lift_placeholder.rs"]
mod indirect_branch_lift_placeholder;

#[path = "indirect_resolve/indirect_resolve_classify.rs"]
mod indirect_resolve_classify;

#[path = "indirect_resolve/indirect_resolve_jump_table.rs"]
mod indirect_resolve_jump_table;

#[path = "indirect_resolve/known_targets_union.rs"]
mod known_targets_union;

#[path = "indirect_resolve/orchestrator_indirect_branch.rs"]
mod orchestrator_indirect_branch;

#[path = "indirect_resolve/orchestrator_indirect_resolution.rs"]
mod orchestrator_indirect_resolution;

#[path = "indirect_resolve/return_target_verification.rs"]
mod return_target_verification;
