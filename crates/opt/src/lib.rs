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
pub mod sp_expr;
mod worklist;
pub use error::Result;
mod call_other_elide;
mod constant_fold;
mod dead_branch;
mod function_args;
pub mod indirect_branch_resolve;
mod known_bits;
mod load_readonly;
mod redundant_phis;
pub mod stack_load_forward;
mod stack_store;

pub use call_other_elide::{CallOtherElide, NO_OP_USER_OPS};
pub use constant_fold::ConstantFold;
pub use dead_branch::DeadBranchElimination;
pub use function_args::FunctionArgDetect;
pub use indirect_branch_resolve::{
    AnchorAddr, AnchorCallingContext, IndirectBranchResolve, ResolvedTargets,
    apply_link_register, apply_tail_call, classify_anchor, classify_anchor_with_rom,
    classify_anchor_with_rom_and_sp, classify_jump_table, classify_stack_array,
    find_placeholder_return_for_anchor,
};
pub use known_bits::{KnownBits, Kb, analyze as analyze_known_bits};
pub use load_readonly::LoadReadOnly;
pub use reader::ReadOnlyMemory;
pub use pipeline::{OptimizationResult, Optimizer, OptimizerPipeline};
pub use redundant_phis::RedundantPhis;
pub use stack_load_forward::StackLoadForward;
pub use stack_store::{CallStackArgCollect, StackStoreDetect};

/// Stable subset of the default pipeline — passes whose rewrites survive
/// the addition of new phi inputs in a later strider fixed-point
/// iteration.  Used while the IR `Graph` is still growing under the
/// indirect-branch resolver's outer loop.
///
/// # Correctness
///
/// Every pass listed here MUST produce IR that is robust against a
/// future predecessor arriving at any region — i.e. it rewrites nodes
/// in place but never *removes* phi / `ControlState` / `If` nodes that
/// the strider [`RegionIrCache`] pins by `NodeId`.  Adding a pass
/// here that detaches dependents would invalidate cached body
/// references in the next iteration.
///
/// See `docs/superpowers/specs/2026-04-27-indirect-branch-fixedpoint-design.md`
/// — section "Stable vs destructive optimizer passes" — for the
/// pass-by-pass rationale.
///
/// Note: [`LoadReadOnly`] is also stable per the spec table but takes
/// a caller-supplied ROM image, so it can't be added with a default
/// configuration.  Callers that have a ROM (e.g. strider's
/// `build_optimizer_pipeline`) layer it on top of this subset.
///
/// Passes (in order):
/// 1. [`ConstantFold`] — operand-rewriting; old nodes become dead but
///    stay alive in the arena.  Phi-input widening doesn't disturb
///    folded successors.
/// 2. [`KnownBits`] — annotation-driven; recomputes from current phi
///    inputs on each run.
#[must_use]
pub fn stable_default_pipeline() -> OptimizerPipeline {
    let mut p = OptimizerPipeline::new();
    // ConstantFold: rewrite-only.  Old operand nodes become dead but
    // are not detached — see spec table row.
    p.add(ConstantFold);
    // KnownBits: bit-level annotation, recomputes per-iteration.
    p.add(KnownBits);
    p
}

/// Destructive subset of the default pipeline — passes that REMOVE
/// nodes from the graph and rewire consumers past them.  Safe to run
/// only after the IR shape is final (i.e. the strider fixed-point
/// loop has converged).
///
/// # Correctness
///
/// Running these passes mid-iteration would invalidate the
/// [`RegionIrCache`] because the cache's pinned phi `NodeId`s and
/// body-side `NodeOutputId`s could point at detached nodes.  The
/// orchestrator runs them exactly once at fixed point.
///
/// Passes (in order):
/// 1. [`RedundantPhis`] — eliminates `ControlPhi` / `MemPhi` /
///    `ControlState` nodes with a single reachable predecessor.
///    Detaches inputs and rewires consumers — destructive.
/// 2. [`DeadBranchElimination`] — removes `If(const)` branches and
///    strips dead control edges.  A later iteration could re-make the
///    condition phi-dependent, but the branch is already gone.
/// 3. [`CallOtherElide`] — drops opaque `CallOther`s whose user-op is
///    a known IR-level no-op (e.g. ARM `setISAMode`).  Treated as
///    destructive for symmetry with the spec table — every node-
///    removal pass is deferred to fixed point.
#[must_use]
pub fn destructive_default_pipeline() -> OptimizerPipeline {
    let mut p = OptimizerPipeline::new();
    p.add(RedundantPhis);
    p.add(DeadBranchElimination);
    p.add(CallOtherElide);
    p
}

/// Builds the default optimizer pipeline containing all built-in passes.
///
/// The pipeline runs all passes in a single shared fixed-point loop: every
/// pass executes once per iteration, and the loop repeats until no pass
/// reports a change.  This means a simplification made by one pass (e.g.
/// folding a condition to `BoolConst(false)`) is immediately visible to later
/// passes in the same iteration and will be propagated further in subsequent
/// iterations without any extra configuration.
///
/// Equivalent to running [`stable_default_pipeline`] followed by
/// [`destructive_default_pipeline`] in order — the two halves' passes
/// are concatenated, preserving the previous default's pass ordering.
///
/// Passes included (in order):
/// 1. [`ConstantFold`] — constant evaluation and algebraic identities
/// 2. [`KnownBits`] — bit-level propagation of known zeros/ones
/// 3. [`RedundantPhis`] — `ControlPhi` / `MemPhi` / `ControlState` elimination
/// 4. [`DeadBranchElimination`] — `If(const)` branch pruning
/// 5. [`CallOtherElide`] — drops opaque `CallOther`s whose user-op name is a
///    known no-op in the IR's value/memory model (e.g. ARM `setISAMode`).
#[must_use]
pub fn default_pipeline() -> OptimizerPipeline {
    let mut p = OptimizerPipeline::new();
    p.add(ConstantFold);
    p.add(KnownBits);
    p.add(RedundantPhis);
    p.add(DeadBranchElimination);
    p.add(CallOtherElide);
    p
}
