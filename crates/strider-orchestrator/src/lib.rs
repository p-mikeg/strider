#![cfg_attr(
    test,
    allow(
        clippy::panic,
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::unreachable
    )
)]

//! Strider orchestrator: drives the lifter / CFG / indirect-branch
//! fixed-point loop and runs the optimization passes.  Hosts the per-region
//! driver, the [`orchestrator::Strider`] analysis handle (its `analyze`
//! method is the top-level entry point), and the cfg-time indirect-resolver
//! stub installed on the cfg builder.  The optimization passes live in the
//! [`strider_opt`] crate, re-exported here as [`opt`] so this crate's public
//! surface stays a superset of the optimizer's.

pub mod orchestrator;

/// The optimization-pass crate, re-exported so downstream consumers can reach
/// passes via `strider_orchestrator::opt::…` alongside the orchestration API.
pub use strider_opt as opt;

pub use orchestrator::{AnalyzeResult, Strider, dump_neighborhood};
/// The CFG→IR lift engine and its option / outcome types, re-exported from
/// `strider-lift` so downstream consumers (the Python bindings, tests) reach
/// them via `strider_orchestrator::…` without a direct `strider-lift` dep.
pub use strider_lift::lift::{LiftOutcome, Lifter};
pub use strider_lift::LiftOptions;
