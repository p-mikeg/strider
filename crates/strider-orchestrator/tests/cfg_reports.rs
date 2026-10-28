//! CFG construction and the incompleteness reports.
//!
//! One binary over `cfg_reports/`, so the shared test tree compiles
//! once for the group instead of once per file.

#[macro_use]
mod common;

#[path = "cfg_reports/bug_on_lifts_cleanly.rs"]
mod bug_on_lifts_cleanly;

#[path = "cfg_reports/cfg_bound_backward_branch.rs"]
mod cfg_bound_backward_branch;

#[path = "cfg_reports/compact.rs"]
mod compact;

#[path = "cfg_reports/incomplete_result_is_returned.rs"]
mod incomplete_result_is_returned;

#[path = "cfg_reports/incompleteness.rs"]
mod incompleteness;

#[path = "cfg_reports/interior_branch_report.rs"]
mod interior_branch_report;

#[path = "cfg_reports/render_determinism.rs"]
mod render_determinism;

#[path = "cfg_reports/undecodable_target_report.rs"]
mod undecodable_target_report;

#[path = "cfg_reports/unmapped_direct_target_report.rs"]
mod unmapped_direct_target_report;
