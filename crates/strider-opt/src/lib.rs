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
//! | [`PhiCollapse`] | Braun trivial-phi elimination on `Phi` / `MemPhi` |
//! | [`RegionCollapse`] | Collapses single-control-input `Region` joins |
//! | [`DeadBranchElimination`] | Folds `If(const)` branches (redirect live successor + detach) |
//! | [`CfgDetach`] | Removes dead `Region`-predecessor slots after a folded `If` |
//!
//! Layered on top by `Strider::build_optimizer_pipeline` (not in
//! `default_pipeline()` because they need calling-convention or ROM data):
//!
//! | Pass | What it does |
//! |------|-------------|
//! | [`LoadReadOnly`] | Folds constant-address loads via a caller-supplied [`ReadOnlyMemory`] |
//! | [`LoadForward`] | Forwards values from SP-relative `Store` to subsequent same-offset `Load` |
//! | [`FunctionArgDetect`] (post-pass) | Registers arg-carrier nodes in the `Function::arg_index_to_values` side-table |
//! | [`CallStackArgCollect`] (post-pass) | Wires positional stack args into `Call` nodes |
//!
//! Indirect-branch resolution is driven separately by the orchestrator
//! (see the crate-internal `indirect_branch_resolve` module); it is not
//! a pipeline pass.

mod alias_mode;
pub mod error;
pub(crate) mod memory_ssa;
pub(crate) mod peephole;
mod pipeline;
pub mod rewrite;
// `pub` so `OptCtx::sp_memo` (a public field of type
// `sp_expr::SpExprMemo`) is reachable through a public path.  Only the
// already-`pub`-re-exported `SpExpr` / `SpExprMemo` / `decompose_sp` /
// `ranges_disjoint` become nameable downstream; the alias-classification
// internals stay `pub(crate)`.
pub mod sp_expr;
mod worklist;
pub use alias_mode::AliasMode;
pub use error::Result;
pub use rewrite::{
    BoxedRule, GraphEditFunctionExt, GraphRewriter, apply_rules_in_order, boxed_rule, rewrite_rule,
    rewrite_rule_runtime,
};
pub use strider_ir::{EditFunction, FunctionState};
mod call_stack_args;
mod cfg_detach;
pub(crate) mod constant_fold;
mod dead_branch;
mod flag_cmp_canonicalize;
mod function_args;
mod if_cond_inversion;
pub mod indirect_branch_resolve;
mod known_bits;
pub(crate) mod load_forward;
mod load_readonly;
mod phi_collapse;
mod region_collapse;
mod stack_offset_detect;
#[cfg(test)]
mod test_support;
pub mod value_range;

pub use call_stack_args::CallStackArgCollect;
pub use cfg_detach::CfgDetach;
pub use constant_fold::ConstantFold;
pub use dead_branch::DeadBranchElimination;
pub use flag_cmp_canonicalize::FlagCmpCanonicalize;
pub use function_args::FunctionArgDetect;
pub use if_cond_inversion::IfCondInversion;
pub use indirect_branch_resolve::{
    classify_anchor, classify_jump_table, classify_stack_array, find_indirect_branch_placeholder,
};
pub use known_bits::{KnownBits, analyze as analyze_known_bits};
#[cfg(test)]
pub(crate) use known_bits::KnownBitsMap;
pub use load_forward::LoadForward;
pub use load_readonly::LoadReadOnly;
pub use phi_collapse::PhiCollapse;
pub use pipeline::{OptCtx, OptimizationResult, Optimizer, OptimizerPipeline, run_one};
pub use region_collapse::RegionCollapse;
pub use stack_offset_detect::StackOffsetDetect;
pub use strider_ir::ReadOnlyMemory;
/// Builds the default optimizer pipeline containing all built-in passes.
///
/// The pipeline runs all passes in a single shared fixed-point loop: every
/// pass executes once per iteration, and the loop repeats until no pass
/// reports a change.  This means a simplification made by one pass (e.g.
/// folding a condition to a false constant (`IntConst(0)` at `I1`)) is
/// immediately visible to later passes in the same iteration and will be
/// propagated further in subsequent iterations without any extra
/// configuration.
///
/// Note: [`LoadReadOnly`] reads its ROM from the per-run [`OptCtx`]
/// rather than from pass state.  `OptimizerPipeline::run` threads the
/// ctx into every pass; callers that ran with `rom = None` see
/// `LoadReadOnly` short-circuit to a no-op.  The PyO3 wrapper
/// auto-prepends a `LoadReadOnly` pass for any custom pipeline that
/// omitted it.
///
/// Nodes that the node-removing passes make fully unreachable are simply
/// left in the arena — the validator and pattern queries only walk from
/// entry, so orphans don't affect correctness and are not swept.
///
/// Passes included (in order):
/// 1. [`ConstantFold`] — constant evaluation and algebraic identities.
/// 2. [`KnownBits`] — bit-level propagation of known zeros/ones.
/// 3. [`FlagCmpCanonicalize`] — flag-tree → single `IntCmpOp` rewrite;
///    runs after `ConstantFold` so `BitNot(BitNot(_))` at `I1` has
///    collapsed first.
/// 4. [`IfCondInversion`] — `If(BitNot(C)) → If(C)` with branches
///    swapped; runs after `FlagCmpCanonicalize` so the cond it sees is
///    at most one `BitNot`-deep, and after `ConstantFold` so a
///    constant-cond `If` is already simplified (swapping branches under
///    a constant cond would make `DeadBranchElimination` strip the
///    wrong arm).
/// 5. [`PhiCollapse`] — Braun trivial-phi elimination on `Phi` / `MemPhi`.
/// 6. [`RegionCollapse`] — collapses single-control-input `Region` joins.
/// 7. [`DeadBranchElimination`] — folds `If(const)` branches: redirects
///    the live successor past the `If` and detaches the folded `If`.
/// 8. [`CfgDetach`] — removes dead `Region`-predecessor slots (and the
///    matching `Phi`/`MemPhi` value slots) once a folded `If` makes a
///    predecessor control-unreachable.
pub fn default_pipeline() -> OptimizerPipeline {
    let mut p = OptimizerPipeline::new();
    p.add(ConstantFold::new());
    p.add(KnownBits);
    p.add(FlagCmpCanonicalize::new());
    p.add(IfCondInversion::new());
    p.add(PhiCollapse);
    p.add(RegionCollapse);
    p.add(DeadBranchElimination);
    p.add(CfgDetach);
    p
}
