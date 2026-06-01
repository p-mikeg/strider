//! IR optimization passes for the Strider binary analysis framework.
//!
//! All passes implement the `Optimizer` trait and report whether they changed
//! the graph via [`OptimizationResult`].  The [`OptimizerPipeline`] runs a list
//! of passes in a shared fixed-point loop: the loop repeats until no pass
//! reports a change in a full iteration.
//!
//! The recommended entry point is [`default_pipeline`], which builds a pipeline
//! containing all built-in passes in their recommended order.
//!
//! # Passes
//!
//! Default pipeline (`default_pipeline()` — fixed-point loop):
//!
//! | Pass | What it does |
//! |------|-------------|
//! | [`ConstantFold`] | Constant evaluation, comparisons, and algebraic identities (`x+0→x`, `x^x→0`, AND-mask merging, …) |
//! | [`KnownBits`] | Bit-level propagation of statically known zeros/ones |
//! | [`FlagCmpCanonicalize`] | Flag-tree → single `IntCmpOp` rewrite (AArch64 NZCV-style flag chains) |
//! | [`IfCondInversion`] | `If(BitNot(C)){A}{B}` → `If(C){B}{A}` |
//! | [`RedundantPhis`] | Eliminates `Phi` / `MemPhi` / `Region` with a single reachable predecessor |
//! | [`DeadBranchElimination`] | Removes `If(const)` branches and strips dead control edges |
//!
//! Layered on top by `Strider::build_optimizer_pipeline` (not in
//! `default_pipeline()` because they need calling-convention or ROM data):
//!
//! | Pass | What it does |
//! |------|-------------|
//! | [`LoadReadOnly`] | Folds constant-address loads via a caller-supplied [`ReadOnlyMemory`] |
//! | [`LoadForward`] | Forwards values from SP-relative `Store` to subsequent same-offset `Load` |
//! | [`FunctionArgDetect`] (post-pass) | Registers arg-carrier nodes in the `Function::arg_index_to_nodes` side-table |
//! | [`CallStackArgCollect`] (post-pass) | Wires positional stack args into `Call` nodes |
//!
//! Indirect-branch resolution is driven separately by the orchestrator
//! (see the crate-internal `indirect_branch_resolve` module); it is not
//! a pipeline pass.

pub mod error;
pub(crate) mod mem_walk;
pub(crate) mod peephole;
mod pipeline;
pub(crate) mod sp_expr;
pub(crate) mod sp_pass_cc;
mod alias_mode;
mod worklist;
pub use alias_mode::AliasMode;
pub use error::Result;
mod cfg_detach;
pub(crate) mod constant_fold;
mod dead_branch;
mod flag_cmp_canonicalize;
mod function_args;
mod if_cond_inversion;
pub(crate) mod indirect_branch_resolve;
mod known_bits;
mod load_readonly;
mod redundant_phis;
pub(crate) mod load_forward;
mod stack_offset_detect;
mod call_stack_args;
#[cfg(test)]
mod test_support;

pub use cfg_detach::CfgDetach;
pub use constant_fold::ConstantFold;
pub use dead_branch::DeadBranchElimination;
pub use flag_cmp_canonicalize::FlagCmpCanonicalize;
pub use function_args::FunctionArgDetect;
pub use if_cond_inversion::IfCondInversion;
pub use indirect_branch_resolve::{
    apply_link_register,
    apply_tail_call,
    classify_anchor,
    classify_jump_table,
    classify_stack_array,
};
pub(crate) use indirect_branch_resolve::{AnchorCallingContext, find_indirect_branch_placeholder};
pub use known_bits::{KnownBits, analyze as analyze_known_bits};
pub(crate) use known_bits::KnownBitsMap;
pub use load_readonly::LoadReadOnly;
pub use strider_ir::ReadOnlyMemory;
pub use pipeline::{OptCtx, OptimizationResult, Optimizer, OptimizerPipeline};
pub use redundant_phis::RedundantPhis;
pub use load_forward::LoadForward;
pub use stack_offset_detect::StackOffsetDetect;
pub use call_stack_args::CallStackArgCollect;
/// Stable subset of the default pipeline — passes whose rewrites survive
/// the addition of new phi inputs in a later strider fixed-point
/// iteration.  Used while the IR `Graph` is still growing under the
/// indirect-branch resolver's outer loop.
///
/// # Correctness
///
/// Every pass listed here MUST produce IR that is robust against a
/// future predecessor arriving at any region — i.e. it rewrites nodes
/// in place but never *removes* phi / `Region` / `If` nodes that
/// the strider orchestrator's per-iteration `RegionIndex` pins by
/// `NodeId`.  Adding a pass here that detaches dependents would
/// invalidate cached body references in the next iteration.
///
/// See `docs/superpowers/specs/2026-04-27-indirect-branch-fixedpoint-design.md`
/// — section "Stable vs destructive optimizer passes" — for the
/// pass-by-pass rationale.
///
/// Note: [`LoadReadOnly`] is also stable per the spec table; it reads
/// its ROM from the per-run [`OptCtx`] rather than from pass state.
/// `OptimizerPipeline::run` threads the ctx into every pass; callers
/// that ran with `rom = None` see `LoadReadOnly` short-circuit to a
/// no-op.  The PyO3 wrapper auto-prepends a `LoadReadOnly` pass for
/// any custom pipeline that omitted it.
///
/// Passes (in order):
/// 1. [`ConstantFold`] — operand-rewriting; old nodes become dead but
///    stay alive in the arena.  Phi-input widening doesn't disturb
///    folded successors.
/// 2. [`KnownBits`] — annotation-driven; recomputes from current phi
///    inputs on each run.
/// 3. [`FlagCmpCanonicalize`] — flag-tree → single `IntCmpOp`
///    rewrite; runs after `ConstantFold` so `BitNot(BitNot(_))` at `I1`
///    has collapsed first.
/// 4. [`IfCondInversion`] — canonicalises `If(BitNot(C))` into
///    `If(C)` with branches swapped; runs after `FlagCmpCanonicalize`
///    so the cond it sees is at most one `BitNot`-deep.
#[must_use]
pub fn stable_default_pipeline() -> OptimizerPipeline {
    let mut p = OptimizerPipeline::new();
    // ConstantFold: rewrite-only.  Old operand nodes become dead but
    // are not detached — see spec table row.
    p.add(ConstantFold);
    // KnownBits: bit-level annotation, recomputes per-iteration.
    p.add(KnownBits);
    // FlagCmpCanonicalize: rewrites flag-tree If conds (AArch64 NZCV-style)
    // into single IntCmpOp shapes against the original `(a, b)`.  Runs
    // before IfCondInversion so the BitNot-wrapped outputs of the LS / GE / LE
    // rules get swapped to direct shape next.
    p.add(FlagCmpCanonicalize);
    // IfCondInversion: canonicalises `If(BitNot(C))` into `If(C)` with
    // branches swapped.  Runs after ConstantFold so the
    // `BitNot(BitNot(x)) → x` rule (at `I1`) has collapsed double negations
    // first, and so any constant-cond `If` (an `IntConst` typed `I1`) has
    // already had its cond simplified (we don't want to swap branches under
    // a constant cond, because `DeadBranchElimination` would then strip the
    // wrong arm).
    p.add(IfCondInversion);
    p
}

/// Destructive subset of the default pipeline — passes that REMOVE
/// nodes from the graph and rewire consumers past them.  Safe to run
/// only after the IR shape is final (i.e. the strider fixed-point
/// loop has converged).
///
/// # Correctness
///
/// Running these passes mid-iteration would invalidate the strider
/// orchestrator's per-iteration `RegionIndex` because its pinned phi
/// `NodeId`s and body-side `NodeOutputId`s could point at detached
/// nodes.  The orchestrator runs them exactly once at fixed point.
///
/// Passes (in order):
/// 1. [`RedundantPhis`] — eliminates `Phi` / `MemPhi` /
///    `Region` nodes with a single reachable predecessor.
///    Detaches inputs and rewires consumers — destructive.
/// 2. [`DeadBranchElimination`] — removes `If(const)` branches and
///    strips dead control edges.  A later iteration could re-make the
///    condition phi-dependent, but the branch is already gone.
#[must_use]
pub fn destructive_default_pipeline() -> OptimizerPipeline {
    let mut p = OptimizerPipeline::new();
    p.add(RedundantPhis);
    p.add(DeadBranchElimination);
    p
}

/// Builds the default optimizer pipeline containing all built-in passes.
///
/// The pipeline runs all passes in a single shared fixed-point loop: every
/// pass executes once per iteration, and the loop repeats until no pass
/// reports a change.  This means a simplification made by one pass (e.g.
/// folding a condition to a false constant (`IntConst(0)` at `I1`)) is immediately visible to later
/// passes in the same iteration and will be propagated further in subsequent
/// iterations without any extra configuration.
///
/// Constructed as [`stable_default_pipeline`] followed by the
/// destructive passes from [`destructive_default_pipeline`] — single
/// source of truth so a future addition to either half lands in
/// `default_pipeline` automatically.
///
/// Passes included (in order):
/// 1. [`ConstantFold`] — constant evaluation and algebraic identities
/// 2. [`KnownBits`] — bit-level propagation of known zeros/ones
/// 3. [`FlagCmpCanonicalize`] — flag-tree → single `IntCmpOp` rewrite
/// 4. [`IfCondInversion`] — `If(BitNot(C)) → If(C)` with branches swapped
/// 5. [`RedundantPhis`] — `Phi` / `MemPhi` / `Region` elimination
/// 6. [`DeadBranchElimination`] — `If(const)` branch pruning
#[must_use]
pub fn default_pipeline() -> OptimizerPipeline {
    let mut p = stable_default_pipeline();
    p.add(RedundantPhis);
    p.add(DeadBranchElimination);
    p
}
