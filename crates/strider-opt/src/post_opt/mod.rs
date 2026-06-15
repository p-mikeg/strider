//! The post-optimization passes — the [`crate::PostOptimizer`] analyses that
//! run ONCE after the fixed-point loop has converged (stack/arg detection and
//! indirect-branch classification), as opposed to the in-loop transforms in
//! [`crate::opt`].  Each submodule owns one post-pass; the types are
//! re-exported at the crate root.

pub(crate) mod call_stack_args;
pub(crate) mod function_args;
pub mod indirect_branch_resolve;
pub(crate) mod stack_offset_detect;
