//! Integration tests for [`strider::ir_cache`] — the persistent
//! per-region IR cache used by the indirect-branch fixed-point loop.
//!
//! Round 1 of the orchestrator rebuilds the CFG and re-lifts the IR
//! from scratch on each iteration; the cache is populated post-hoc
//! to record per-region predecessor counts and start addresses.
//! These tests pin the contract the cache currently honours:
//!
//!   * the cache key is the region's machine start address (stable
//!     across CFG rebuilds),
//!   * `lift_new_regions_into` populates one entry per CFG region,
//!   * `count_uncached_regions` correctly reports new regions across
//!     a 2-stage rebuild,
//!   * `predecessor_diffs` flags regions whose pred count changed,
//!   * `extend_predecessors_into` is a no-op in round 1 (the
//!     orchestrator rebuilds rather than incrementally extending).
//!
//! Future rounds (incremental rebuild — see spec's "Out-of-scope"
//! section) will add per-region IR-handle plumbing, at which point
//! this test file extends with handle-stability and phi-extension
//! cases.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use cfg::{Builder, OptionsBuilder};
use rsleigh::Sleigh;
use rsleigh::mem_readers::BufMemReader;
use std::collections::HashMap;
use strider::{
    cache_key_for_region, count_uncached_regions, extend_predecessors_into,
    lift_new_regions_into, predecessor_diffs, CallingConvention, RegionIrCache, RegionIrEntry,
    SleighArch, Strider,
};

/// Builds a tiny x86_64 function with a single `ret` instruction.
/// Smallest possible CFG: 1 region.
fn build_single_region_setup() -> (Strider, cfg::Cfg<BufMemReader<Vec<u8>>>) {
    let base = 0x1000u64;
    let arch = SleighArch::x86_64();
    let bytes = vec![0xc3u8]; // ret
    let reader = BufMemReader::new(bytes, base);
    let sleigh = Sleigh::new(arch.sla_spec, arch.pspec, reader).expect("sleigh");
    let opts = OptionsBuilder::new().build();
    let cfg = Builder::with_endianness(sleigh, base, opts, arch.endianness)
        .build()
        .expect("cfg build");

    let probe = BufMemReader::new(Vec::<u8>::new(), 0);
    let regs = Sleigh::new(arch.sla_spec, arch.pspec, probe)
        .expect("probe sleigh")
        .regs()
        .expect("probe regs");
    let strider =
        Strider::new(arch, regs, CallingConvention::x86_64_systemv_abi()).expect("strider");
    (strider, cfg)
}

#[test]
fn cache_key_for_region_uses_machine_addr() {
    let (_, cfg) = build_single_region_setup();
    // Pin: the cache key is `MachineInsnAddr`, which round-trips
    // through the cfg's `Region::start_addr.machine_addr`.  Two
    // regions with the same machine address would collide — but a
    // single function only has one region per machine address.
    for region_id in cfg.region_ids() {
        let key = cache_key_for_region(&cfg, region_id).expect("key");
        let region = cfg.graph.node_weight(region_id).expect("region");
        assert_eq!(key, region.start_addr.machine_addr);
    }
}

#[test]
fn lift_into_empty_cache_populates_one_entry_per_region() {
    let (strider, cfg) = build_single_region_setup();
    let mut cache: RegionIrCache = HashMap::new();
    let _outcome =
        lift_new_regions_into(&strider, &mut cache, &cfg).expect("lift_new_regions_into");

    let cfg_region_count = cfg.region_ids().count();
    assert_eq!(
        cache.len(),
        cfg_region_count,
        "cache must have one entry per CFG region",
    );
    assert!(cfg_region_count >= 1, "function has at least one region");
}

#[test]
fn lift_into_full_cache_repopulates_same_keys() {
    // Round 1: the orchestrator clears + repopulates the cache per
    // iteration.  After two consecutive lifts the cache size matches
    // the CFG region count exactly (no orphan entries).
    let (strider, cfg) = build_single_region_setup();
    let mut cache: RegionIrCache = HashMap::new();
    let _ = lift_new_regions_into(&strider, &mut cache, &cfg).expect("first lift");
    let _ = lift_new_regions_into(&strider, &mut cache, &cfg).expect("second lift");
    assert_eq!(cache.len(), cfg.region_ids().count());
}

#[test]
fn count_uncached_regions_zero_after_full_lift() {
    let (strider, cfg) = build_single_region_setup();
    let mut cache: RegionIrCache = HashMap::new();
    let _ = lift_new_regions_into(&strider, &mut cache, &cfg).expect("lift");
    let n = count_uncached_regions(&cfg, &cache).expect("count");
    assert_eq!(n, 0, "after a full lift, no regions should be uncached");
}

#[test]
fn count_uncached_regions_equals_total_for_empty_cache() {
    let (_, cfg) = build_single_region_setup();
    let cache: RegionIrCache = HashMap::new();
    let n = count_uncached_regions(&cfg, &cache).expect("count");
    assert_eq!(n, cfg.region_ids().count());
}

#[test]
fn predecessor_diffs_empty_after_consistent_lift() {
    // After `lift_new_regions_into` populates the cache from the
    // current CFG, predecessor_diffs returns an empty vec — the
    // cache's cached_predecessor_count matches the CFG.
    let (strider, cfg) = build_single_region_setup();
    let mut cache: RegionIrCache = HashMap::new();
    let _ = lift_new_regions_into(&strider, &mut cache, &cfg).expect("lift");
    let diffs = predecessor_diffs(&cfg, &cache).expect("diff");
    assert!(diffs.is_empty(), "no diffs after a fresh lift; got {diffs:?}");
}

#[test]
fn predecessor_diffs_flags_artificial_regression() {
    // Pin the diff path by hand-rolling a cache whose
    // cached_predecessor_count is intentionally too small.  The diff
    // function must report (key, cached, current).  This exercises
    // the diff branch without needing a real "CFG that gained an
    // edge across two iterations" — that's a future-rounds concern.
    let (_, cfg) = build_single_region_setup();
    let mut cache: RegionIrCache = HashMap::new();
    for region_id in cfg.region_ids() {
        let key = cache_key_for_region(&cfg, region_id).expect("key");
        let region = cfg.graph.node_weight(region_id).expect("region");
        let mut entry = RegionIrEntry::empty(region.start_addr);
        // Set cached_predecessor_count to a sentinel value that is
        // not the real count.
        entry.cached_predecessor_count = 99;
        cache.insert(key, entry);
    }
    let diffs = predecessor_diffs(&cfg, &cache).expect("diff");
    // Single-region function: 1 diff (the entry region had 0 preds
    // in reality but we recorded 99).
    assert_eq!(diffs.len(), 1);
    let (_, cached, current) = diffs[0];
    assert_eq!(cached, 99);
    assert_ne!(current, 99);
}

#[test]
fn extend_predecessors_into_no_new_preds_is_noop() {
    // Round-1 contract: `extend_predecessors_into` is a no-op (the
    // orchestrator rebuilds rather than incrementally extending).
    // The function must therefore return Ok(()) and leave the cache
    // untouched.  Pinning this prevents a future refactor from
    // accidentally introducing IR mutation in round 1.
    let (strider, cfg) = build_single_region_setup();
    let mut cache: RegionIrCache = HashMap::new();
    let _ = lift_new_regions_into(&strider, &mut cache, &cfg).expect("lift");
    let cache_before = cache.clone();
    extend_predecessors_into(&mut cache, &cfg).expect("extend");
    assert_eq!(
        cache.len(),
        cache_before.len(),
        "extend_predecessors_into is a no-op in round 1",
    );
    for (k, before) in &cache_before {
        let after = cache.get(k).expect("key still present");
        assert_eq!(before.cached_predecessor_count, after.cached_predecessor_count);
        assert_eq!(before.start_addr, after.start_addr);
    }
}

#[test]
fn cache_key_stable_across_rebuilds() {
    // Build the same function twice and assert the cache keys are
    // bit-identical.  This is the round-1 stand-in for the future
    // "incremental rebuild reuses cache entries" contract: the keys
    // are stable, so a future round that switches from rebuild-from-
    // scratch to incremental-rebuild can rely on
    // `cache.contains_key(k)` to short-circuit re-lifting.
    let (strider1, cfg1) = build_single_region_setup();
    let mut cache1: RegionIrCache = HashMap::new();
    let _ = lift_new_regions_into(&strider1, &mut cache1, &cfg1).expect("lift");

    let (strider2, cfg2) = build_single_region_setup();
    let mut cache2: RegionIrCache = HashMap::new();
    let _ = lift_new_regions_into(&strider2, &mut cache2, &cfg2).expect("lift");

    let mut keys1: Vec<_> = cache1.keys().copied().collect();
    let mut keys2: Vec<_> = cache2.keys().copied().collect();
    keys1.sort();
    keys2.sort();
    assert_eq!(keys1, keys2);
}
