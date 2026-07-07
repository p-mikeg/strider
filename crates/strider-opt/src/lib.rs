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
//! | [`LoadReadOnly`] | Folds constant-address loads via the per-run [`OptCtx`]'s [`ReadOnlyMemory`] (no-ops without a ROM) |
//! | [`KnownBits`] | Bit-level propagation of statically known zeros/ones |
//! | [`FlagCmpCanonicalize`] | Flag-tree → single `IntCmpOp` rewrite (AArch64 NZCV-style flag chains) |
//! | [`IfCondInversion`] | `If(Xor(C,1):I1){A}{B}` → `If(C){B}{A}` |
//! | [`PhiCollapse`] | Braun trivial-phi elimination on `Phi` / `MemPhi` |
//! | [`RegionCollapse`] | Collapses single-control-input `Region` joins |
//! | [`DeadBranchElimination`] | Folds `If(const)` branches (redirect live successor + detach) |
//! | [`CfgDetach`] | Removes dead `Region`-predecessor slots after a folded `If` |
//! | [`LoadForward`] | Forwards values from SP-relative `Store` to subsequent same-offset `Load` |
//! | [`StackOffsetDetect`] (post-pass) | Stamps SP-relative `Store`/`Load` offsets in the `Function::stack_offsets` side-table |
//! | [`CallStackArgCollect`] (post-pass) | Wires positional stack args into `Call` nodes |
//! | [`FunctionArgDetect`] (post-pass) | Registers arg-carrier nodes in the `Function::arg_index_to_values` side-table |
//!
//! The SP-aware passes read their calling convention from the function's
//! own `default_cc` and their alias precision from the per-run [`OptCtx`];
//! `LoadReadOnly` reads its ROM from the [`OptCtx`].  All take no
//! construction arguments and no-op when their input is absent, so the
//! one `default_pipeline()` covers every run.
//!
//! Indirect-branch resolution is driven separately by the orchestrator
//! (see the crate-internal `indirect_branch_resolve` module); it is not
//! a pipeline pass.

pub mod error;
// Crate-internal: the payload-agnostic backward memory-SSA walk (the
// `MemorySSAWalker` trait + DFS engine), driven by `sp_analysis`'s alias walker.
// No downstream crate names it, so it stays `pub(crate)`.
pub(crate) mod mem_ssa;
mod options;
pub(crate) mod peephole;
mod pipeline;
pub mod rewrite_rule;
// Crate-internal: the SP-expression decomposition lives here and its results
// are cached on the function's `stack_offsets` side-table — no downstream crate
// names `SpExpr` / `ranges_disjoint`, so the whole module stays `pub(crate)`.
pub(crate) mod sp_analysis;
pub use error::Result;
pub use options::{AliasMode, MemAliasOptions, OptOptions};
pub use rewrite_rule::{
    BoxedRule, apply_rules_count, apply_rules_in_order, rewrite_rule, rewrite_rule_runtime,
};
pub use strider_ir::{EditFunction, FunctionState};

/// Shared "node → constant from constant inputs" utility (single ROM-decode +
/// per-op fold dispatch site shared by `LoadReadOnly` and the jump-table
/// abstract evaluator).
mod const_eval;
/// In-loop optimization passes (graph→graph transforms run in the fixed-point
/// loop).
mod opt;
/// Converged-graph post-passes (run once after the loop).
mod post_opt;
#[cfg(test)]
mod test_support;
pub mod value_range;

// `indirect_branch_resolve` keeps a public module path (downstream reaches its
// classifiers); the rest of the passes surface only through their re-exported
// pass types below.
pub use post_opt::indirect_branch_resolve;

pub use opt::cfg_detach::CfgDetach;
pub use opt::constant_fold::ConstantFold;
pub use opt::dead_branch::DeadBranchElimination;
pub use opt::flag_cmp_canonicalize::FlagCmpCanonicalize;
pub use opt::if_cond_inversion::IfCondInversion;
#[cfg(test)]
pub(crate) use opt::known_bits::KnownBitsMap;
pub use opt::known_bits::{KnownBits, analyze as analyze_known_bits};
pub use opt::load_forward::LoadForward;
pub use opt::load_readonly::LoadReadOnly;
pub use opt::phi_collapse::PhiCollapse;
pub use opt::region_collapse::RegionCollapse;
pub use pipeline::{OptCtx, OptimizationResult, Optimizer, OptimizerPipeline, PostOptimizer};
#[cfg(any(test, feature = "test-util"))]
pub use pipeline::{run_one, run_post};
pub use post_opt::call_stack_args::CallStackArgCollect;
pub use post_opt::function_args::FunctionArgDetect;
pub use post_opt::indirect_branch_resolve::{IndirectBranchClassify, classify_target};
pub use post_opt::stack_offset_detect::StackOffsetDetect;
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
///    runs after `ConstantFold` so the doubled `Xor(Xor(_,1),1)` at `I1` has
///    collapsed first.
/// 4. [`IfCondInversion`] — `If(Xor(C,1):I1) → If(C)` with branches
///    swapped; runs after `FlagCmpCanonicalize` so the cond it sees is
///    at most one `Xor(_,1)`-deep, and after `ConstantFold` so a
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
    // Fold constant-address loads from the caller-supplied ROM (read from
    // the per-run `OptCtx`; no-ops when no ROM is present).  Placed after
    // ConstantFold so a folded constant address is available, and inside
    // the loop so the value it materialises feeds the next iteration's
    // folding.
    p.add(LoadReadOnly);
    p.add(KnownBits);
    p.add(FlagCmpCanonicalize::new());
    p.add(IfCondInversion::new());
    p.add(PhiCollapse);
    p.add(RegionCollapse);
    // (Structural twins a rewrite leaves behind — e.g. `PhiCollapse` redirecting
    // two SSA phis to the same value, leaving two identical `Truncate`/`Add`
    // nodes — are now re-merged incrementally by `EditFunction::clean()`'s
    // re-canonicalization at every pass boundary, so no dedup pass is needed.)
    p.add(DeadBranchElimination);
    p.add(CfgDetach);
    // SP-relative store→load forwarding runs in the fixed-point loop; it
    // reads its calling convention from the function's `default_cc` and
    // alias precision from the per-run `OptCtx`, so it needs no
    // construction arguments and no-ops cleanly when neither is meaningful.
    p.add(LoadForward);
    // Post-passes (run once after the loop converges, in this order):
    // StackOffsetDetect stamps SP-relative Store/Load offsets, which
    // CallStackArgCollect then consumes — so StackOffsetDetect must come
    // first.  All three are classification / wiring on the converged graph.
    p.add_post_pass(StackOffsetDetect);
    p.add_post_pass(CallStackArgCollect);
    p.add_post_pass(FunctionArgDetect);
    p
}
