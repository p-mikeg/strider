//! IR optimization passes.
//!
//! Passes implement `Optimizer` and report whether they changed the graph via
//! [`OptimizationResult`]. [`OptimizerPipeline`] runs them in a shared
//! fixed-point loop that repeats until a full iteration reports no change,
//! then runs post-passes once on the converged graph. [`default_pipeline`] is
//! the canned pass list.
//!
//! No pass takes construction arguments: SP-aware passes read the calling
//! convention from the function's own `default_cc` and alias precision from
//! the per-run [`OptCtx`], and `LoadReadOnly` reads its ROM from that ctx.
//! Each no-ops when its input is absent, so one `default_pipeline()` covers
//! every run.
//!
//! Indirect-branch resolution is driven by the orchestrator (see
//! `indirect_branch_resolve`); it is not a pipeline pass.

pub mod error;
pub(crate) mod mem_ssa;
mod options;
pub(crate) mod peephole;
mod pipeline;
pub mod rewrite_rule;
pub(crate) mod sp_analysis;
pub use error::Result;
pub use options::{AliasMode, MemAliasOptions, OptOptions};
pub use rewrite_rule::{
    BoxedRule, apply_rules_count, apply_rules_in_order, rewrite_rule, rewrite_rule_runtime,
};
pub use strider_ir::{EditFunction, FunctionState};

mod const_eval;
/// Passes that run inside the fixed-point loop.
mod opt;
/// Passes that run once, after the loop converges.
mod post_opt;
#[cfg(test)]
mod test_support;
pub mod value_range;

// Public module path: downstream reaches the classifiers directly, not just
// the pass type.
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

/// Every pass runs once per fixed-point iteration, so one pass's
/// simplification is visible to the rest within the same iteration.
///
/// Order constraints:
/// - `FlagCmpCanonicalize` after `ConstantFold`, so a doubled
///   `Xor(Xor(_,1),1)` at `I1` has already collapsed.
/// - `IfCondInversion` after both, so the cond it sees is at most one
///   `Xor(_,1)` deep and a constant-cond `If` is already simplified.
///   Swapping branches under a constant cond would make
///   `DeadBranchElimination` strip the wrong arm.
///
/// `LoadReadOnly` reads its ROM from the per-run [`OptCtx`]; with `rom =
/// None` it short-circuits to a no-op. The PyO3 wrapper auto-prepends it to
/// any custom pipeline that omitted it.
///
/// Nodes the node-removing passes strand are left in the arena: the validator
/// and pattern queries only walk from entry.
pub fn default_pipeline() -> OptimizerPipeline {
    let mut p = OptimizerPipeline::new();
    p.add(ConstantFold::new());
    // After ConstantFold so a folded constant address is available, and
    // inside the loop so the value it materialises feeds the next iteration.
    p.add(LoadReadOnly);
    p.add(KnownBits);
    p.add(FlagCmpCanonicalize::new());
    p.add(IfCondInversion::new());
    p.add(PhiCollapse);
    p.add(RegionCollapse);
    // No dedup pass: structural twins a rewrite leaves behind (e.g.
    // PhiCollapse redirecting two SSA phis to the same value) are re-merged
    // by `EditFunction::clean()` at every pass boundary.
    p.add(DeadBranchElimination);
    p.add(CfgDetach);
    p.add(LoadForward);
    // CallStackArgCollect consumes the SP-relative offsets StackOffsetDetect
    // stamps, so that order is required.
    p.add_post_pass(StackOffsetDetect);
    p.add_post_pass(CallStackArgCollect);
    p.add_post_pass(FunctionArgDetect);
    p
}
