//! Region-lift drivers — the entry points the orchestrator calls each
//! fixed-point iteration to materialise CFG regions into IR.

use cfg::{Cfg, MachineInsnAddr};

use crate::error::Result;

use super::entry::RegionIrEntry;
use super::stats::LiftStats;
use super::{cache_key_for_region, RegionIrCache};

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
    // Discard stats — callers who need them go through
    // `lift_new_regions_into_with_stats`.
    let (outcome, _stats) = lift_new_regions_into_with_stats(strider, cache, cfg)?;
    Ok(outcome)
}

/// Variant of [`lift_new_regions_into`] that also returns a
/// [`LiftStats`] reporting how many regions / pcode insns the
/// **cache contract** considers newly lifted by this call.
///
/// CORRECTNESS — pre-call snapshot: we snapshot `cache.keys()` BEFORE
/// invoking the strider lift.  Any region in `cfg` whose
/// `MachineInsnAddr` is in that pre-snapshot is considered cached
/// (and contributes 0 to the lift counters).  Any region not in the
/// pre-snapshot is considered freshly lifted.
///
/// CORRECTNESS — pcode count source: the count is taken from
/// `cfg.graph[region_id].insns.len()` of the freshly-lifted regions
/// — i.e. the actual number of pcode instructions the lift would
/// have to process for those regions.  This matches the round-2
/// semantic where each pcode insn is lifted at most once.
///
/// # Errors
///
/// Propagates `analyze_cfg` errors.
pub fn lift_new_regions_into_with_stats<R: rsleigh::MemReader>(
    strider: &crate::Strider,
    cache: &mut RegionIrCache,
    cfg: &Cfg<R>,
) -> Result<(crate::AnalyzeOutcome, LiftStats)> {
    // CORRECTNESS — pre-call cache snapshot: capture which regions
    // were cached BEFORE the lift so we can distinguish "newly
    // lifted" from "already cached" after the lift completes.
    let cached_pre: std::collections::HashSet<MachineInsnAddr> =
        cache.keys().copied().collect();

    // Identify freshly-lifted regions BEFORE actually running the
    // lift — we walk `cfg` to compute (region_id, machine_addr,
    // insn_count) for each region, then mark those whose
    // `machine_addr` is NOT in the pre-snapshot as "fresh".
    let mut stats = LiftStats::default();
    for region_id in cfg.region_ids() {
        let key = cache_key_for_region(cfg, region_id)?;
        if cached_pre.contains(&key) {
            // CORRECTNESS — cached: contributes zero to the lift
            // counters.  See round-1 vs round-2 note on `LiftStats`.
            continue;
        }
        let region = cfg
            .graph
            .node_weight(region_id)
            .ok_or(crate::error::ErrorKind::CfgNoRegion(region_id))?;
        stats.regions_lifted += 1;
        stats.pcode_insns_lifted += region.insns.len();
        stats.newly_lifted_addrs.push(key);
    }

    // CORRECTNESS — full re-lift this iteration: round 1 doesn't
    // persist the FunctionBuilder, so we physically re-lift
    // everything.  The returned `region_handles` snapshot is
    // captured against the freshly-built graph and replaces any
    // prior cache entries — but the LiftStats above already pinned
    // the round-2-compatible "new regions only" count.
    let outcome = strider.analyze_cfg(cfg)?;
    populate_cache_from_handles(cache, &outcome.region_handles);
    Ok((outcome, stats))
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
