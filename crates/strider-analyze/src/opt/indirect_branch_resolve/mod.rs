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
//!   [`strider_lift::cfg::ResolvedTargets`] ([`classify_anchor`]).
//! - [`inplace`] — in-place IR edits for `LinkRegister` returns and
//!   `Single` tail calls (`apply_link_register`, `apply_tail_call`).
//! - [`jump_table`] — rodata jump-table arm.
//! - [`stack_array`] — stack-array-of-labels arm.
//!
//! ## Where `ResolvedTargets` lives
//!
//! Defined in `strider_lift::cfg::builder::indirect_resolver` (the
//! lowest layer that needs the enum: it's the return type of the
//! [`strider_lift::cfg::IndirectResolverFn`] callback the cfg builder
//! hands to its installed resolver).  Import it directly from there.

#![allow(clippy::module_name_repetitions)]

use strider_ir::Graph;
use strider_ir::node::{NodeId, NodeKind, ValueId, ValueKind};

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

/// Cast a u128 IR constant to a 64-bit **branch target**.  Returns
/// `None` when the high 64 bits are non-zero — those constants are
/// never valid jump targets on any 64-bit ISA, and silently truncating
/// would produce a wrong CFG edge.  Use this anywhere an `IntConst(u128)`
/// flows directly into a CFG target slot (`ResolvedTargets::Single`,
/// `Multiple`, jump-table base addresses).
#[inline]
#[must_use]
pub(crate) fn u128_to_branch_target(k: u128) -> Option<u64> {
    u64::try_from(k).ok()
}

pub use classify::classify_anchor;
pub use inplace::{apply_link_register, apply_tail_call};
pub use jump_table::classify_jump_table;
pub use stack_array::classify_stack_array;

/// Per-anchor calling-convention snapshot consumed by the in-place
/// editors ([`apply_link_register`] / [`apply_tail_call`]).  The
/// orchestrator populates this from the cache's `exit_vn_to_value` for
/// the dispatch region; the in-place editors thread it into the
/// resulting Call/Return nodes.
#[derive(Debug, Clone, Default)]
pub struct AnchorCallingContext {
    /// IR `ValueId` for the calling convention's stack-pointer varnode
    /// at the dispatch site.  Threaded as `inputs[3]` to the resulting
    /// Call node (the SP anchor, ahead of the args).
    pub sp_value: Option<ValueId>,
    /// IR `ValueId`s for the calling convention's
    /// `arg_passing_vars` at the dispatch site.  Threaded as
    /// `inputs[4..]` to the resulting Call node (slots after control,
    /// memory, target, sp).
    pub arg_passing_values: Vec<ValueId>,
    /// `ValueKind`s for the calling convention's clobbered
    /// varnodes at the dispatch site.  Threaded as the Call node's
    /// value outputs after `[Control, Memory]`.
    pub clobbered_kinds: Vec<ValueKind>,
    /// IR `ValueId`s for the calling convention's `ret_val_regs`
    /// at the dispatch site.  Threaded as the resulting Return node's
    /// inputs after `[control, memory, target_value]`
    /// (link-register case) or `[call_ctrl, call_mem]` (tail-call
    /// case).
    pub ret_val_values: Vec<ValueId>,
}

/// Walk the use-list of `anchor_value` and return the unique
/// 3-input `IndirectBranch` whose `target_value` input equals
/// `anchor_value` — the placeholder shape pinned at strider's lift
/// time.
///
/// Returns `None` when no such placeholder exists (e.g. an earlier
/// in-place edit already replaced it: `apply_tail_call` detaches the
/// node, and `apply_link_register` mutates the kind to
/// [`NodeKind::Return`]).  Public so strider's orchestrator can reuse
/// the same lookup for its own bookkeeping.
#[must_use]
pub fn find_indirect_branch_placeholder(
    graph: &Graph,
    anchor_value: ValueId,
) -> Option<NodeId> {
    for (consumer, _input_index) in graph.value_uses(anchor_value) {
        if !matches!(graph.node_kind(consumer), NodeKind::IndirectBranch) {
            continue;
        }
        // `IndirectBranch` has the signature `[control, memory,
        // target_value]` (see `node_signature::expected_signature`), so
        // slot 2 (target) is guaranteed once the kind is established
        // (validated structural invariant).
        let [_, _, val] = graph.node_inputs_exact::<3>(consumer)
            .expect("IndirectBranch has 3 inputs (validated)");
        if val == anchor_value {
            return Some(consumer);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::u128_to_branch_target;

    #[test]
    fn u128_to_branch_target_passes_through_u64_range_values() {
        assert_eq!(u128_to_branch_target(0), Some(0));
        assert_eq!(u128_to_branch_target(0x1234_5678), Some(0x1234_5678));
        assert_eq!(u128_to_branch_target(u128::from(u64::MAX)), Some(u64::MAX));
    }

    #[test]
    fn u128_to_branch_target_rejects_high_bits_set() {
        // First u128 value above u64::MAX.
        assert_eq!(u128_to_branch_target(u128::from(u64::MAX) + 1), None);
        // Both high bits set.
        assert_eq!(u128_to_branch_target(u128::MAX), None);
        // High 64 bits set, low 64 bits zero — common wide-const shape.
        assert_eq!(u128_to_branch_target(1u128 << 64), None);
    }
}
