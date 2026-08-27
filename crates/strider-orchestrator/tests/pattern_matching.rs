//! Pattern queries whose fixtures need a strider-opt pass (`PhiCollapse`,
//! `IfCondInversion`, `FunctionArgDetect`) applied first.  They cannot live in
//! `crates/strider-pattern/tests/` with the rest: strider-pattern would have to
//! depend on strider-opt, inverting the crate graph.

#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::useless_conversion
)]

#[path = "pattern_matching/support/mod.rs"]
mod support;

#[path = "pattern_matching/cast_mask_walk.rs"]
mod cast_mask_walk;

#[path = "pattern_matching/if_pat_symmetric.rs"]
mod if_pat_symmetric;

#[path = "pattern_matching/ssa.rs"]
mod ssa;
