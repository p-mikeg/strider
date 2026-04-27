//! Producer-shape classifier for tier-2 indirect-branch resolution.
//!
//! See module docs in `super` for the high-level role.  The classifier
//! is implemented in R2.2 (IntConst + InitialVar(lr)) and R2.3
//! (ValuePhi); R2.1 only exposes the function name so the rest of the
//! module compiles.

use cfg::test_api::ResolvedTargets;
use ir::BuiltFunctionGraph;
use ir::node::NodeOutputId;

/// Classify a placeholder anchor's producer node into a
/// [`ResolvedTargets`].  Returns `None` when the producer doesn't
/// match any of the known sound shapes — the orchestrator (R3)
/// interprets `None` as "still unresolved at this iteration; try
/// again or surface as `UnresolvedIndirectBranch` at fixed point."
///
/// `link_register_vn` is the calling convention's link register
/// varnode (`None` on stack-push ABIs like x86 / x86_64 where there
/// is no architectural link register).
///
/// # Round R2.1 status
///
/// This is the module-skeleton placeholder.  All shapes return
/// `None`; the real arms land in R2.2 / R2.3.
#[must_use]
pub fn classify_anchor(
    _graph: &BuiltFunctionGraph,
    _anchor_output: NodeOutputId,
    _link_register_vn: Option<rsleigh::Vn>,
) -> Option<ResolvedTargets> {
    // R2.1: module skeleton only.  The real classifier lands in R2.2.
    None
}
