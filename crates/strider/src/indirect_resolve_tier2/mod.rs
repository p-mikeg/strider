//! Tier-2 (post-IR) resolver for `BranchIndirect` placeholders that
//! tier 1 (the cfg-time mini-graph in `cfg::indirect_resolve`) couldn't
//! classify.
//!
//! Tier 2 inspects the producer of each placeholder's anchored target
//! value AFTER the stable optimiser subset has run on the full IR.
//! This gives it visibility into cross-region flow, `StackLoadForward`
//! results, `LoadReadOnly` resolutions, and `KnownBits` propagation —
//! none of which the single-region tier 1 mini-graph can see.
//!
//! ## Public surface
//!
//! [`classify_anchor`] is the producer-shape classifier exposed for the
//! orchestrator (lands in R3).  It takes the optimised function graph
//! plus the placeholder anchor's value-output and returns
//! `Some(ResolvedTargets)` when it can soundly classify the indirect
//! branch's target set, or `None` to let the orchestrator defer the
//! branch to a later iteration / surface it as
//! [`cfg::ErrorKind::UnresolvedIndirectBranch`] at fixed point.

pub use cfg::test_api::ResolvedTargets;

mod classify;
pub mod inplace;
pub mod orchestrator;
// The jump-table extension lands in R4.

pub use classify::classify_anchor;
pub use inplace::{apply_link_register, apply_tail_call};
pub use orchestrator::{run as run_orchestrator, OrchestratorConfig};
