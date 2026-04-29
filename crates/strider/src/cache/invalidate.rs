//! Cache invalidation — detects regions whose pcode-content has shrunk
//! between two CFG iterations (a region was split mid-way) and evicts
//! the now-stale cache entries so the next lift repopulates them from
//! scratch.

use cfg::Cfg;

use crate::error::Result;

use super::{cache_key_for_region, RegionIrCache};

/// Detect regions in `old_cfg` that have been split in `new_cfg` and
/// evict their cache entries.  A split is detected when:
///
///   * A region with start_addr A and N instructions in `old_cfg`,
///   * Has the same start_addr A in `new_cfg` but FEWER than N
///     instructions (some pcode moved into the new second-half region
///     when a branch landed mid-region).
///
/// CORRECTNESS NOTE — round-1 invalidation strategy: the original IR
/// nodes for the post-split first half remain in the persistent
/// `Graph` arena — they're stale w.r.t. the new region but harmless
/// because `lift_new_regions_into` will re-lift the first half from
/// pcode in the next call.  The validator's reachability scope skips
/// the now-zombie nodes that previously belonged to the second half.
/// See spec section "Stale cache invalidation" for the surgical
/// alternative documented as future work (per-insn boundary handles,
/// splice the body at the split point).
///
/// CORRECTNESS NOTE — uncached new-CFG regions: regions that exist in
/// `new_cfg` but have no cache entry are LEFT ALONE — there is
/// nothing to invalidate.  `lift_new_regions_into` will lift them as
/// fresh entries on the next pass.
///
/// CORRECTNESS NOTE — old regions absent from new_cfg: regions cached
/// from a prior CFG that simply no longer exist in `new_cfg` (e.g. a
/// dead-branch elimination removed them — though round-1 doesn't run
/// destructive on intermediate iterations) are also LEFT ALONE
/// because their cache key isn't iterated.  This is fine: the next
/// rebuild repopulates the cache from the new CFG and old entries
/// effectively shadow.  A future round may add an "evict-orphan"
/// pass; round-1 doesn't need it because cache iteration is keyed on
/// `new_cfg.region_ids()` end-to-end.
///
/// # Errors
///
/// Propagates [`crate::error::ErrorKind::CfgNoRegion`] if either CFG
/// reports a region id that has no node weight (a malformed graph;
/// the cfg builder never produces this in practice).
pub fn invalidate_split_regions<R: rsleigh::MemReader>(
    cache: &mut RegionIrCache,
    old_cfg: &Cfg<R>,
    new_cfg: &Cfg<R>,
) -> Result<()> {
    // Walk new_cfg's regions; for each one whose key is in `cache`,
    // compare its insn count against the old_cfg region with the same
    // start_addr.  A shrunk insn count signals a split.
    for region_id in new_cfg.region_ids() {
        let key = cache_key_for_region(new_cfg, region_id)?;
        if !cache.contains_key(&key) {
            // CORRECTNESS: brand-new region — nothing cached, nothing
            // to invalidate.  See "uncached new-CFG regions" above.
            continue;
        }
        let new_region = new_cfg
            .graph
            .node_weight(region_id)
            .ok_or_else(|| anyhow::anyhow!("no region {region_id:?} in cfg"))?;
        let new_insn_count = new_region.insns.len();
        // Find the old region with the same start_addr.  If none exists
        // (rare — would mean the new key was never in the old CFG, but
        // we have a cache entry for it), skip — the lift_new_regions_into
        // call will handle the freshness check.
        let Some(old_region_id) = old_cfg.region_id_at_start(key) else {
            // CORRECTNESS: cache says we lifted this region before but
            // the old CFG doesn't have it.  Either the cache is stale
            // (e.g. carried over from a much earlier CFG) or this is
            // a brand-new region whose key collides with an evicted
            // one.  Either way, leave the cache entry alone — the
            // next lift will overwrite if needed.
            continue;
        };
        let old_region = old_cfg
            .graph
            .node_weight(old_region_id)
            .ok_or_else(|| anyhow::anyhow!("no region {old_region_id:?} in cfg"))?;
        let old_insn_count = old_region.insns.len();
        if new_insn_count < old_insn_count {
            // CORRECTNESS: shrunk insn count — split detected.  Evict
            // the cache entry; the next `lift_new_regions_into` will
            // re-lift the now-shorter first half from pcode.
            cache.remove(&key);
        }
    }
    Ok(())
}
