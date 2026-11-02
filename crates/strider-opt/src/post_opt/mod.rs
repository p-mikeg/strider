//! Analyses that run ONCE after the fixed-point loop converges
//! ([`crate::OptimizerPipeline::add_post_pass`]).

pub(crate) mod call_stack_args;
pub(crate) mod function_args;
pub(crate) mod indirect_branch_resolve;
