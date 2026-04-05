mod opt;
mod utils;
mod redundant_selectors;
mod constant_fold;
mod known_bits;
mod dead_branch;
mod load_readonly;

pub use opt::{OptimizationResult, OptimizerPipeline, Optimizer};
pub use redundant_selectors::RedundantSelectors;
pub use constant_fold::ConstantFold;
pub use known_bits::KnownBits;
pub use dead_branch::DeadBranchElimination;
pub use load_readonly::{ReadOnlyMemory, LoadReadOnly};

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
/// 3. [`RedundantSelectors`] — phi / ControlState elimination
/// 4. [`DeadBranchElimination`] — `If(const)` branch pruning
pub fn default_pipeline() -> OptimizerPipeline {
    let mut p = OptimizerPipeline::new();
    p.add(ConstantFold);
    p.add(KnownBits);
    p.add(RedundantSelectors);
    p.add(DeadBranchElimination);
    p
}
