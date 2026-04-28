//! BUG-30 stack-array arm — F5 shim.  The canonical implementation
//! lives in [`opt::indirect_branch_resolve::stack_array`].  This
//! module preserves the original strider-level API for back-compat.

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
    opt::classify_stack_array(&graph.graph, anchor_output, stack_ptr_vn).map(|r| match r {
        opt::BranchResolution::LinkRegister => ResolvedTargets::LinkRegister,
        opt::BranchResolution::Single(k) => ResolvedTargets::Single(k),
        opt::BranchResolution::Multiple(ts) => ResolvedTargets::Multiple(ts),
    })
}
