//! IR optimization passes for the Strider binary analysis framework.
//!
//! All passes implement the [`Optimizer`] trait and report whether they changed
//! the graph via [`OptimizationResult`].  The [`OptimizerPipeline`] runs a list
//! of passes in a shared fixed-point loop: the loop repeats until no pass
//! reports a change in a full iteration.
//!
//! The recommended entry point is [`default_pipeline`], which builds a pipeline
//! containing all built-in passes in their recommended order.
//!
//! # Passes
//!
//! | Pass | What it does |
//! |------|-------------|
//! | [`ConstantFold`] | Constant evaluation, comparisons, and algebraic identities (`x+0→x`, `x^x→0`, …) |
//! | [`KnownBits`] | Bit-level propagation of statically known zeros/ones |
//! | [`RedundantPhis`] | Eliminates `ControlPhi`, `MemPhi`, and `ControlState` nodes with a single reachable predecessor |
//! | [`DeadBranchElimination`] | Removes `If(const)` branches and strips dead control edges |
//! | [`LoadReadOnly`] | Folds constant-address loads by reading from a caller-supplied read-only memory region |

#![cfg_attr(
    test,
    allow(
        clippy::panic,
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::unreachable
    )
)]

extern crate self as opt;

pub mod error;
mod pipeline;
mod sp_expr;
pub use error::{Error, ErrorKind, Result};
mod constant_fold;
mod dead_branch;
mod function_args;
mod known_bits;
mod load_readonly;
mod redundant_phis;
mod stack_load_forward;
mod stack_store;

pub use constant_fold::ConstantFold;
pub use dead_branch::DeadBranchElimination;
pub use function_args::FunctionArgDetect;
pub use known_bits::KnownBits;
pub use load_readonly::LoadReadOnly;
pub use reader::ReadOnlyMemory;
pub use pipeline::{OptimizationResult, Optimizer, OptimizerPipeline};
pub use redundant_phis::RedundantPhis;
pub use stack_load_forward::StackLoadForward;
pub use stack_store::{CallStackArgCollect, StackStoreDetect};

/// Builds the default optimizer pipeline containing all built-in passes.
///
/// The pipeline runs all passes in a single shared fixed-point loop: every
/// pass executes once per iteration, and the loop repeats until no pass
/// reports a change.  This means a simplification made by one pass (e.g.
/// folding a condition to `BoolConst(false)`) is immediately visible to later
/// passes in the same iteration and will be propagated further in subsequent
/// iterations without any extra configuration.
///
/// Passes included (in order):
/// 1. [`ConstantFold`] — constant evaluation and algebraic identities
/// 2. [`KnownBits`] — bit-level propagation of known zeros/ones
/// 3. [`RedundantPhis`] — `ControlPhi` / `MemPhi` / `ControlState` elimination
/// 4. [`DeadBranchElimination`] — `If(const)` branch pruning
pub fn default_pipeline() -> OptimizerPipeline {
    let mut p = OptimizerPipeline::new();
    p.add(ConstantFold);
    p.add(KnownBits);
    p.add(RedundantPhis);
    p.add(DeadBranchElimination);
    p
}
