pub type Result<T> = anyhow::Result<T>;

pub(crate) mod mem_analysis;
pub(crate) mod mem_ssa;
pub(crate) mod peephole;
mod pipeline;
pub(crate) mod rewrite_rule;
pub use mem_analysis::refresh_memory_classes;
pub use rewrite_rule::{BoxedRule, apply_rules_count, rewrite_rule, rewrite_rule_runtime};
pub use strider_ir::AssumptionOptions;
pub use strider_ir::EditFunction;

mod const_eval;
#[cfg(test)]
mod differential;
/// Passes that run inside the fixed-point loop.
mod opt;
/// Passes that run once, after the loop converges.
mod post_opt;
#[cfg(test)]
mod test_support;

pub use opt::constant_fold::ConstantFold;
pub use opt::dead_branch::DeadBranchElimination;
pub use opt::flag_cmp_canonicalize::FlagCmpCanonicalize;
pub use opt::if_cond_inversion::IfCondInversion;
pub use opt::known_bits::KnownBits;
pub use opt::load_forward::LoadForward;
pub use opt::load_readonly::LoadReadOnly;
pub use opt::phi_collapse::PhiCollapse;
pub use opt::region_collapse::RegionCollapse;
pub use pipeline::{OptCtx, Optimizer, OptimizerPipeline};
#[cfg(any(test, feature = "test-util"))]
pub use pipeline::{run_one, run_one_unvalidated, run_post};
pub use post_opt::call_stack_args::CallStackArgCollect;
pub use post_opt::function_args::FunctionArgDetect;
#[cfg(any(test, feature = "test-util"))]
pub use post_opt::indirect_branch_resolve::classify_target;
pub use post_opt::indirect_branch_resolve::{ClassifiedSites, ReturnTargets, classify_sites};
pub use strider_ir::ReadOnlyMemory;

pub fn default_pipeline() -> OptimizerPipeline {
    let mut p = OptimizerPipeline::new();
    p.add(&ConstantFold);
    // After ConstantFold so a folded constant address is available, and
    // inside the loop so the value it materialises feeds the next iteration.
    p.add(&LoadReadOnly);
    p.add(&KnownBits);
    // After ConstantFold, so a doubled `Xor(Xor(_,1),1)` at `I1` has already
    // collapsed.
    p.add(&FlagCmpCanonicalize);
    // After both, so the cond a match sees is at most one `Xor(_,1)` deep:
    // ConstantFold collapses the doubled xor and FlagCmpCanonicalize rewrites
    // the flag trees that would otherwise bury one.
    p.add(&IfCondInversion);
    p.add(&PhiCollapse);
    p.add(&RegionCollapse);
    p.add(&DeadBranchElimination);
    p.add(&LoadForward);
    p.add_post_pass(&CallStackArgCollect);
    p.add_post_pass(&FunctionArgDetect);
    p
}
