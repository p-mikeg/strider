#![cfg_attr(
    test,
    allow(
        clippy::panic,
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::unreachable
    )
)]

//! Thin shim re-exporting the strider analysis facade from
//! [`strider_analyze`].
//!
//! Phase 3 Task 3.1c moved the orchestrator (`run` / `RunConfig`), the
//! IR-level indirect-branch resolver, the [`GraphRewriter`], and the
//! [`Strider`] per-iteration handle into `strider-analyze`.  This crate
//! keeps the well-known `strider::*` import path (`strider::run`,
//! `strider::Strider`, …) working for callers — primarily strider-py
//! and the workspace integration test suite — without changing their
//! source.
//!
//! # Key types (re-exports)
//!
//! - [`Strider`] / [`run`] — top-level analysis entry points
//! - [`SleighArch`] / [`CallingConvention`] — architecture + ABI selection
//! - [`GraphRewriter`] — pattern-rewrite façade
//! - [`UnresolvedIndirectBranch`] — typed error returned by [`run`]

pub use strider_analyze::indirect_resolve;
pub use strider_analyze::rewrite;
pub use strider_analyze::{
    AnalyzeOptions, AnalyzeOutcome, GraphRewriter, RegionLiftHandles, RunConfig, Strider,
    UnresolvedIndirectBranch, run,
};
pub use target::{BuiltCallingConvention, CallingConvention, Endianness, SleighArch};
