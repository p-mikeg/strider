//! IR-level indirect-branch resolver.
//!
//! Classifies placeholder anchors that the strider lifter inserts at
//! `BranchIndirect` sites.  The strider orchestrator drives the outer loop
//! (CFG rebuild, cache invalidation, iteration cap) and calls into the
//! classifier functions directly — there is no opt-pipeline pass for
//! indirect-branch resolution.
//!
//! ## Submodules
//!
//! - [`classify`] — producer-shape classifier ([`classify_anchor`])
//!   returning [`strider_cfg::ResolvedTargets`], plus the
//!   analysis-only post-pass that drives it over every live
//!   `IndirectBranch` placeholder ([`IndirectBranchClassify`]).
//! - [`table`] — unified table-dispatch arm covering both the rodata
//!   jump-table (absolute base) and on-stack label-array (SP-rooted base)
//!   shapes ([`classify_table_dispatch`]).
//!
//! ## Where `ResolvedTargets` lives
//!
//! Defined in `strider_cfg::indirect_resolver` (the
//! lowest layer that needs the enum: the cfg builder consumes it via
//! `LiftOptions::known_targets` to seat indirect-branch terminators, and
//! it is the return type of [`classify_anchor`] itself).  Import it directly
//! from there.

#![allow(clippy::module_name_repetitions)]

use strider_ir::Graph;
use strider_ir::node::{NodeId, NodeKind, ValueId};

pub mod classify;
pub mod table;

/// Per-anchor enumeration cap for the table-dispatch arm
/// (`table::classify_table_dispatch`), covering both the rodata jump-table
/// (absolute base) and on-stack label-array (SP-rooted base) shapes.
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
pub fn u128_to_branch_target(k: u128) -> Option<u64> {
    u64::try_from(k).ok()
}

pub use classify::{IndirectBranchClassify, classify_anchor};
pub use table::classify_table_dispatch;

/// Walk the use-list of `anchor_value` and return the unique
/// 3-input `IndirectBranch` whose `target_value` input equals
/// `anchor_value` — the placeholder shape pinned at strider's lift
/// time.
///
/// Returns `None` when no such placeholder exists.  Public so the
/// orchestrator can locate the placeholder for bookkeeping.
#[must_use]
pub fn find_indirect_branch_placeholder(graph: &Graph, anchor_value: ValueId) -> Option<NodeId> {
    for (consumer, _input_index) in graph.value_uses(anchor_value) {
        if !matches!(graph.node_kind(consumer), NodeKind::IndirectBranch) {
            continue;
        }
        // `IndirectBranch` has the signature `[control, memory,
        // target_value]` (see `node_signature::expected_signature`), so
        // slot 2 (target) is guaranteed once the kind is established
        // (validated structural invariant).
        let [_, _, val] = graph
            .node_inputs_exact::<3>(consumer)
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
