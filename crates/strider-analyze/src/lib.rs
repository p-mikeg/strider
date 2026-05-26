#![cfg_attr(
    test,
    allow(
        clippy::panic,
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::unreachable
    )
)]

//! Strider analyze: optimization passes, pattern matching, indirect-branch
//! resolution, and the orchestrator.  Consolidates the optimizer, pattern
//! matcher, per-region driver, and `Strider` entry point into one crate.

pub mod orchestrator;
pub mod rewrite;
mod strider;

pub mod indirect_resolver;
pub mod opt;
pub mod pattern;

pub use orchestrator::{dump_neighborhood, dump_per_region, run, Config};
pub use rewrite::GraphRewriter;
pub use strider::{AnalyzeOptions, AnalyzeOutcome, Strider};
