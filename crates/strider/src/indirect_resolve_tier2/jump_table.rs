//! Jump-table arm — F5 shim.  Delegates to
//! [`opt::classify_jump_table`].  Retained as a strider-side entry
//! point so the orchestrator and the integration tests can call into
//! the classifier under a stable strider path; the underlying logic
//! lives in [`opt::indirect_branch_resolve::jump_table`].

use cfg::test_api::ResolvedTargets;
use ir::BuiltFunctionGraph;
use ir::node::NodeOutputId;
use opt::ReadOnlyMemory;

/// Top-level classifier hook for the jump-table arm.  Delegates to
/// [`opt::classify_jump_table`].
#[must_use]
pub fn classify_jump_table(
    graph: &BuiltFunctionGraph,
    anchor_output: NodeOutputId,
    rom: Option<&dyn ReadOnlyMemory>,
    link_register_vn: Option<rsleigh::Vn>,
) -> Option<ResolvedTargets> {
    opt::classify_jump_table(graph, anchor_output, rom, link_register_vn)
}
