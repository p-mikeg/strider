//! ISA-mode commits across the resolve loop.
//!
//! One binary over `isa_mode/`, so the shared test tree compiles
//! once for the group instead of once per file.

#[macro_use]
mod common;

#[path = "isa_mode/isa_clash_drops_one_arm.rs"]
mod isa_clash_drops_one_arm;

#[path = "isa_mode/isa_clash_reports_the_site_anchor.rs"]
mod isa_clash_reports_the_site_anchor;

#[path = "isa_mode/isa_mode_conflict_report.rs"]
mod isa_mode_conflict_report;

#[path = "isa_mode/isa_mode_repin_on_reused_lifter.rs"]
mod isa_mode_repin_on_reused_lifter;

#[path = "isa_mode/isa_mode_resolved_branch.rs"]
mod isa_mode_resolved_branch;

#[path = "isa_mode/isa_mode_survives_a_split.rs"]
mod isa_mode_survives_a_split;
