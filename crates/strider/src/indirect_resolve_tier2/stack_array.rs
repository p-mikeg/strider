//! BUG-30 stack-array arm — F5 shim.  Delegates to
//! [`opt::classify_stack_array`].  Retained as a strider-side entry
//! point so the orchestrator and integration tests can call into the
//! classifier under a stable strider path; the underlying logic lives
//! in [`opt::indirect_branch_resolve::stack_array`].

use cfg::test_api::ResolvedTargets;
use ir::BuiltFunctionGraph;
use ir::node::NodeOutputId;

/// Top-level classifier hook for the stack-array arm.  Delegates to
/// [`opt::classify_stack_array`].
#[must_use]
pub fn classify_stack_array(
    graph: &BuiltFunctionGraph,
    anchor_output: NodeOutputId,
    stack_ptr_vn: rsleigh::Vn,
) -> Option<ResolvedTargets> {
    opt::classify_stack_array(graph, anchor_output, stack_ptr_vn)
}
