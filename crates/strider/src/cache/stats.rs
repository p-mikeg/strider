//! `LiftStats` — measures how much "lifting work" a single
//! [`crate::cache::lift_new_regions_into_with_stats`] call represents
//! under the cache contract.
//!
//! CORRECTNESS NOTE — round-1 vs. cache contract: in round 1 the lift
//! physically rebuilds the IR each call (no persistent FunctionBuilder
//! across iterations).  But the spec's "every pcode instruction is
//! lifted to IR at most once across the entire fixed-point analysis"
//! contract is **measurable** at this API surface: we count only the
//! regions that did NOT exist in the cache prior to the call, since
//! those are the regions a future round-2 (with a persistent IR
//! graph) would actually lift.  Cached regions, even though they're
//! physically re-lifted in round 1's fresh FunctionBuilder, do NOT
//! count toward `pcode_insns_lifted` — round 2 will literally skip
//! them, and the cache-contract test pins the round-2 semantic at
//! the API level.

use cfg::MachineInsnAddr;

/// Reports of how much "lifting work" a [`crate::cache::lift_new_regions_into`]
/// call represents under the cache contract — see
/// [`crate::cache::lift_new_regions_into_with_stats`] for the precise
/// semantics.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LiftStats {
    /// Number of regions whose cache entry was freshly populated by
    /// this call (i.e. they were not in the cache pre-call).  After a
    /// `cache.clear()` followed by a lift, this equals
    /// `cfg.region_ids().count()`.
    pub regions_lifted: usize,
    /// Sum of `pcode_insn_count` over all freshly-lifted regions.
    /// Cached regions contribute zero, mirroring the round-2 semantic.
    pub pcode_insns_lifted: usize,
    /// The machine start addresses of the newly-lifted regions, in
    /// the order they appeared in the CFG iteration.  Tests use this
    /// to pin which regions the lift decided were new.
    pub newly_lifted_addrs: Vec<MachineInsnAddr>,
}
