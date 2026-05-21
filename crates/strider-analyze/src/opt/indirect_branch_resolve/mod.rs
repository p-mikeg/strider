//! IR-level indirect-branch resolver.
//!
//! Classifies placeholder anchors that the strider lifter inserts at
//! `BranchIndirect` sites and exposes the in-place IR edits for the
//! resolutions that don't require a CFG rebuild.  The strider
//! orchestrator drives the outer loop (CFG rebuild, cache invalidation,
//! iteration cap) and calls into the classifier + inplace functions
//! directly — there is no opt-pipeline pass for indirect-branch
//! resolution.
//!
//! ## Submodules
//!
//! - [`classify`] — producer-shape classifier returning
//!   [`ResolvedTargets`] ([`classify_anchor`]).
//! - [`inplace`] — in-place IR edits for `LinkRegister` returns and
//!   `Single` tail calls (`apply_link_register`, `apply_tail_call`).
//! - [`jump_table`] — rodata jump-table arm.
//! - [`stack_array`] — stack-array-of-labels arm.
//!
//! ## Where [`ResolvedTargets`] lives
//!
//! Defined in `strider_lift::cfg::builder::indirect_resolver` (the
//! lowest layer that needs the enum: it's the return type of the
//! [`strider_lift::cfg::IndirectResolverFn`] callback the cfg builder
//! hands to its installed resolver).  Re-exported here so pre-existing
//! call sites that import `opt::ResolvedTargets` keep working.

#![allow(clippy::module_name_repetitions)]

use strider_ir::Graph;
use strider_ir::node::{NodeId, NodeKind, NodeOutputId, NodeOutputKind};

pub mod classify;
pub mod inplace;
pub mod jump_table;
pub mod stack_array;

/// Per-anchor enumeration cap, shared by both the rodata jump-table arm
/// (`jump_table::classify_jump_table`) and the stack-array-of-labels arm
/// (`stack_array::classify_stack_array`).
///
/// `u32::MAX + 1` if a known-bits mask were all-ones, so without this cap
/// a buggy KnownBits result could force iteration through 4 GiB of slots.
/// Real jump tables emitted by gcc/clang are bounded by the source-level
/// `switch` arm count, almost always well under 4096.  Tables larger than
/// this cap are unusual enough that we prefer `None` (defer to
/// `UnresolvedIndirectBranch`) over the pathological enumeration cost.
pub(crate) const MAX_TABLE_ENTRIES: u64 = 4096;

pub use classify::classify_anchor;
pub use inplace::{apply_link_register, apply_tail_call};
pub use jump_table::classify_jump_table;
pub use stack_array::classify_stack_array;

/// Re-export of the canonical [`ResolvedTargets`] enum, which now lives
/// in `strider-lift` so the cfg builder's
/// [`strider_lift::cfg::IndirectResolverFn`] callback can return it
/// without forming a dep cycle.
pub use strider_lift::cfg::ResolvedTargets;

/// Per-anchor calling-convention snapshot consumed by the in-place
/// editors ([`apply_link_register`] / [`apply_tail_call`]).  The
/// orchestrator populates this from the cache's `exit_vn_to_value` for
/// the dispatch region; the in-place editors thread it into the
/// resulting Call/Return nodes.
#[derive(Debug, Clone, Default)]
pub struct AnchorCallingContext {
    /// IR `NodeOutputId`s for the calling convention's
    /// `arg_passing_vars` at the dispatch site.  Threaded as
    /// `inputs[3..]` to the resulting Call node (slots after control,
    /// memory, target).
    pub arg_passing_outputs: Vec<NodeOutputId>,
    /// `NodeOutputKind`s for the calling convention's clobbered
    /// varnodes at the dispatch site.  Threaded as the Call node's
    /// value outputs after `[Control, Memory]`.
    pub clobbered_kinds: Vec<NodeOutputKind>,
    /// IR `NodeOutputId`s for the calling convention's `ret_val_regs`
    /// at the dispatch site.  Threaded as the resulting Return node's
    /// inputs after `[control, memory, target_value]`
    /// (link-register case) or `[call_ctrl, call_mem]` (tail-call
    /// case).
    pub ret_val_outputs: Vec<NodeOutputId>,
}

/// Walk the use-list of `anchor_output` and return the unique
/// 3-input `IndirectBranch` whose `target_value` input equals
/// `anchor_output` — the placeholder shape pinned at strider's lift
/// time.
///
/// Returns `None` when no such placeholder exists (e.g. an earlier
/// in-place edit already replaced it: `apply_tail_call` detaches the
/// node, and `apply_link_register` mutates the kind to
/// [`NodeKind::Return`]).  Public so strider's orchestrator can reuse
/// the same lookup for its own bookkeeping.
#[must_use]
pub fn find_placeholder_return_for_anchor(
    graph: &Graph,
    anchor_output: NodeOutputId,
) -> Option<NodeId> {
    for (consumer, _input_index) in graph.output_uses(anchor_output) {
        if !matches!(graph.node_kind(consumer), NodeKind::IndirectBranch) {
            continue;
        }
        // `IndirectBranch` has the signature `[control, memory,
        // target_value]` (see `node_signature::expected_signature`),
        // so `node_inputs_exact::<3>` is structurally guaranteed to
        // succeed; the `Ok(...)` arm is the only reachable branch.
        if let Ok([_, _, val]) = graph.node_inputs_exact::<3>(consumer)
            && val == anchor_output
        {
            return Some(consumer);
        }
    }
    None
}
