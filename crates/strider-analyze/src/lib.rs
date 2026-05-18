//! Strider analyze: optimization passes, pattern matching, indirect-branch
//! resolution, and the orchestrator. v2 consolidation of opt + pattern
//! (Phase 3.0) and the strider orchestrator (Phase 3.1).

#![cfg_attr(
    test,
    allow(
        clippy::panic,
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::unreachable
    )
)]

pub mod opt;
