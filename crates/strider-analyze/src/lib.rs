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
//! resolution, and the orchestrator. v2 consolidation of opt + pattern
//! (Phase 3.0) and the strider orchestrator + per-region driver +
//! `Strider` (Phase 3.1c).

mod errors;
pub mod orchestrator;
pub mod rewrite;
mod strider;

pub mod indirect_resolve;
pub mod indirect_resolver;
pub mod opt;
pub mod pattern;

pub use errors::UnresolvedIndirectBranch;
pub use orchestrator::{run, Config};
pub use rewrite::GraphRewriter;
pub use strider::{AnalyzeOptions, AnalyzeOutcome, RegionLiftHandles, Strider};
pub use target::{BuiltCallingConvention, CallingConvention, Endianness, SleighArch};
