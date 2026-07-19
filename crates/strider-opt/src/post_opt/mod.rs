//! [`crate::PostOptimizer`] analyses that run ONCE after the fixed-point loop
//! converges, unlike the in-loop transforms in [`crate::opt`].  One post-pass
//! per submodule; the types are re-exported at the crate root.

pub(crate) mod call_stack_args;
pub(crate) mod function_args;
pub mod indirect_branch_resolve;
pub(crate) mod stack_offset_detect;
