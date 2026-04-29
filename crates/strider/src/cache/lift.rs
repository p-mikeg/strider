//! Region-lift drivers — the entry points the orchestrator calls each
//! fixed-point iteration to materialise CFG regions into IR.

use cfg::Cfg;

use crate::error::Result;

use super::entry::RegionIrEntry;
use super::RegionIrCache;

/// CORRECTNESS NOTE — `lift_new_regions_into`:
///
/// Lifts every uncached region in `cfg` into the IR graph (via
/// [`crate::Strider::analyze_cfg`]) and populates `cache`
/// with one [`RegionIrEntry`] per CFG region.  Cached regions in
/// `cfg` keep their existing entries — populating overwrites the
/// previous handles only when the snapshot was captured against the
/// same iteration's lift.
///
/// The cache-hit branch is correct because the body of a region
/// depends on (i) earlier-in-region nodes wired at lift time
/// (deterministic from the pcode), and (ii) the region's entry-
/// boundary phi `NodeId`s.  Both are pinned in `RegionIrEntry` and
/// remain valid even when a new predecessor arrives — predecessor
/// extension only adds INPUTS to existing phi nodes, never moves or
/// removes them, so body refs stay valid.
///
/// # Round-1 architecture caveat
///
/// In round 1 the strider lift constructs a fresh `FunctionBuilder`
/// each call.  This means cached entries from a PREVIOUS iteration's
/// graph do NOT survive into the new graph (the `NodeId`s point into
/// the old arena).  The orchestrator handles this by clearing the
/// cache on each rebuild and repopulating from the new lift's
/// snapshot.  Future rounds will persist the builder, at which point
/// cache hits truly skip re-lifting.
///
/// # Errors
///
/// Propagates the strider lift's errors verbatim.
pub fn lift_new_regions_into<R: rsleigh::MemReader>(
    strider: &crate::Strider,
    cache: &mut RegionIrCache,
    cfg: &Cfg<R>,
) -> Result<crate::AnalyzeOutcome> {
    // CORRECTNESS — full re-lift this iteration: round 1 doesn't
    // persist the FunctionBuilder, so we physically re-lift
    // everything.  The returned `region_handles` snapshot is
    // captured against the freshly-built graph and replaces any
    // prior cache entries.
    let outcome = strider.analyze_cfg(cfg)?;
    populate_cache_from_handles(cache, &outcome.region_handles);
    Ok(outcome)
}

/// Populates `cache` with one [`RegionIrEntry`] per
/// [`crate::RegionLiftHandles`] in `handles`, keyed by the region's
/// machine start address.  Overwrites prior entries — see the
/// `lift_new_regions_into` correctness note for round-1's reset
/// semantics.
pub(super) fn populate_cache_from_handles(
    cache: &mut RegionIrCache,
    handles: &[crate::RegionLiftHandles],
) {
    for h in handles {
        // CORRECTNESS — overwrite-on-rebuild: round 1 rebuilds the IR
        // arena each iteration, so the prior handles are stale.
        // Always overwrite — the rebuild contract guarantees the new
        // handles are consistent with the new graph.
        let entry = RegionIrEntry::from_lift_handles(h);
        cache.insert(h.start_addr.machine_addr, entry);
    }
}
