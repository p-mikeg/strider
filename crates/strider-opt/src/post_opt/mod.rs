//! [`crate::PostOptimizer`] analyses that run ONCE after the fixed-point loop
//! converges.

pub(crate) mod call_stack_args;
pub(crate) mod function_args;
pub mod indirect_branch_resolve;
pub(crate) mod stack_offset_detect;
