//! Jump-table arm — F5 shim.  The canonical implementation lives in
//! [`opt::indirect_branch_resolve::jump_table`].  This module
//! preserves the original strider-level API
//! (`BuiltFunctionGraph` argument, `cfg::ResolvedTargets` return)
//! for back-compat with existing tests and shims.

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
    opt::classify_jump_table(graph, anchor_output, rom, link_register_vn).map(|r| {
        match r {
            opt::BranchResolution::LinkRegister => ResolvedTargets::LinkRegister,
            opt::BranchResolution::Single(k) => ResolvedTargets::Single(k),
            opt::BranchResolution::Multiple(ts) => ResolvedTargets::Multiple(ts),
        }
    })
}
