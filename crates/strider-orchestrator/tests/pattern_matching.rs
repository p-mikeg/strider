//! Integration tests for `strider_pattern` that exercise optimizer-pass
//! interactions specific to strider-orchestrator.
//!
//! The bulk of pattern-only tests now live in `crates/strider-pattern/tests/`.
//! What remains here are pattern queries whose fixtures need a
//! strider-opt optimizer pass (PhiCollapse, IfCondInversion,
//! FunctionArgDetect) applied before the pattern runs; moving them out
//! would require strider-pattern to depend on strider-orchestrator and
//! invert the crate graph.

#![allow(
    clippy::panic,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::unreachable,
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
