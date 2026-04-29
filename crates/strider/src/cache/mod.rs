//! Per-region IR cache used by the indirect-branch fixed-point loop.
//!
//! Each entry pins, for one CFG region, the IR-side handles the
//! orchestrator and the in-place editors need:
//!
//!   * the entry / exit control & memory `NodeOutputId`s,
//!   * the per-var `ControlPhi` `NodeId`s at the entry boundary, so a
//!     future iteration that brings a new predecessor can append an
//!     input to the *existing* phi nodes (preserving body refs),
//!   * the `MemPhi` and `ControlState` `NodeId`s for the same reason,
//!   * the exit `vn_to_value` map for downstream consumers.
//!
//! # Cache key
//!
//! The cache key is `MachineInsnAddr` — the machine address of the
//! region's first instruction.  This is **stable across CFG rebuilds**:
//! the same machine address always lifts to the same pcode, so the
//! same key always identifies the same body.  Region splits
//! (`split_region` in cfg) preserve the first half's start address;
//! the second half gets a fresh address.  The key handles both halves
//! transparently.
//!
//! # Cache lifetime
//!
//! Cache entries hold raw `NodeId` / `NodeOutputId` values that index
//! into a specific `BuiltFunctionGraph`'s arena.  An entry is valid
//! ONLY for the graph it was populated against — calling
//! `lift_new_regions_into` against a fresh CFG produces fresh entries
//! that supersede prior ones.  The orchestrator clears or rebuilds
//! the cache when it discards the underlying graph.
//!
//! # Module structure (W6)
//!
//! The original `ir_cache.rs` (~1200 lines) splits into four focused
//! submodules.  Each owns a coherent slice of the cache lifecycle:
//!
//!   * [`entry`] — the [`RegionIrEntry`] / [`PredecessorHandles`]
//!     types.
//!   * [`lift`] — the lift driver ([`lift_new_regions_into`]).
//!   * [`extend`] — predecessor-edge extension
//!     ([`extend_predecessors_into`],
//!     [`extend_predecessors_with_handle`]).
//!   * [`invalidate`] — split detection
//!     ([`invalidate_split_regions`]).
//!
//! Top-level helpers ([`cache_key_for_region`],
//! [`count_uncached_regions`], [`predecessor_diffs`]) and the
//! [`RegionIrCache`] alias stay here in `mod.rs` because they touch
//! every submodule and have no natural sub-home.

use std::collections::HashMap;

use cfg::{Cfg, MachineInsnAddr, RegionId};

use crate::error::Result;

mod entry;
mod extend;
mod invalidate;
mod lift;

#[cfg(test)]
mod tests;

pub use entry::{PredecessorHandles, RegionIrEntry};
pub use extend::{extend_predecessors_into, extend_predecessors_with_handle};
pub use invalidate::invalidate_split_regions;
pub use lift::lift_new_regions_into;

/// Persistent map from a region's machine start address to its IR
/// boundary handles.  Keyed by `MachineInsnAddr` (not `PcodeInsnAddr`)
/// because regions always start at a machine-instruction boundary —
/// the pcode index is implicitly 0 for region starts.
pub type RegionIrCache = HashMap<MachineInsnAddr, RegionIrEntry>;

/// The cache key for a CFG region.  Returns the machine address of
/// the region's first instruction.
///
/// # Errors
///
/// Returns `crate::error::ErrorKind::CfgNoRegion` wrapped in
/// `crate::Error` when `region_id` does not refer to a region in
/// `cfg`.  Stable for the lifetime of the CFG.
pub fn cache_key_for_region<R: rsleigh::MemReader>(
    cfg: &Cfg<R>,
    region_id: RegionId,
) -> Result<MachineInsnAddr> {
    let region = cfg
        .graph
        .node_weight(region_id)
        .ok_or(crate::error::ErrorKind::CfgNoRegion(region_id))?;
    Ok(region.start_addr.machine_addr)
}

/// Counts how many regions in `cfg` are absent from `cache`.
///
/// Used by the orchestrator to size the work that "lift only new
/// regions" would have to do; in round-1 this is informational
/// because the orchestrator re-lifts from scratch each iteration.
///
/// # Errors
///
/// Propagates `cache_key_for_region`'s error if any region in `cfg`
/// has no entry in `cfg.graph`.
pub fn count_uncached_regions<R: rsleigh::MemReader>(
    cfg: &Cfg<R>,
    cache: &RegionIrCache,
) -> Result<usize> {
    let mut n = 0;
    for region_id in cfg.region_ids() {
        let key = cache_key_for_region(cfg, region_id)?;
        if !cache.contains_key(&key) {
            n += 1;
        }
    }
    Ok(n)
}

/// Counts how many CFG predecessors each cached region has, returning
/// `Some(diff)` when any region's current predecessor count exceeds
/// the cached count (signalling a new predecessor edge has been added
/// in this iteration's CFG).  Returns `None` when no diffs.
///
/// Used by `extend_predecessors_into`-style call sites to short-
/// circuit when nothing new has arrived.
///
/// # Errors
///
/// Propagates `cache_key_for_region`'s error.
pub fn predecessor_diffs<R: rsleigh::MemReader>(
    cfg: &Cfg<R>,
    cache: &RegionIrCache,
) -> Result<Vec<(MachineInsnAddr, usize, usize)>> {
    let mut diffs = Vec::new();
    for region_id in cfg.region_ids() {
        let key = cache_key_for_region(cfg, region_id)?;
        if let Some(entry) = cache.get(&key) {
            let preds = cfg.predecessor_count(region_id);
            if preds != entry.cached_predecessor_count {
                diffs.push((key, entry.cached_predecessor_count, preds));
            }
        }
    }
    Ok(diffs)
}
