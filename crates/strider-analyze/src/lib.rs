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

mod errors;
pub mod orchestrator;
pub mod rewrite;
mod strider;

pub mod indirect_resolver;
pub mod opt;
pub mod pattern;

pub use errors::UnresolvedIndirectBranch;
pub use orchestrator::{run, Config};
pub use rewrite::GraphRewriter;
pub use strider::{AnalyzeOptions, AnalyzeOutcome, RegionLiftHandles, Strider};
pub use strider_target::{BuiltCallingConvention, CallingConvention, Endianness, SleighArch};
