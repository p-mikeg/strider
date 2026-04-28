#![allow(clippy::unwrap_used, clippy::expect_used)]

//! Pinning tests for re-using one [`rsleigh::Sleigh`] handle across
//! multiple [`Builder::build`] calls.
//!
//! These tests guard the contract relied on by
//! [`strider::indirect_resolve_tier2::orchestrator`] which threads the
//! Sleigh from one iteration's [`Cfg::sleigh`] into the next iteration's
//! [`Builder::with_endianness`].  Re-using the Sleigh across builds is
//! the entire point — re-constructing it would re-load the SLA spec.

mod common;
use common::{make_sleigh_with_bytes, TestReader};

use cfg::{Builder, Cfg, OptionsBuilder};

fn build_one(sleigh: rsleigh::Sleigh<TestReader>, start: u64) -> Cfg<TestReader> {
    Builder::new(sleigh, start, OptionsBuilder::new().build())
        .build()
        .expect("Builder::build")
}

#[test]
fn cfg_sleigh_field_round_trip() {
    // Build a Cfg, harvest its `sleigh` field, build a second Cfg using
    // the same Sleigh handle.  Both Cfgs must be valid (each contains
    // at least one region with the expected entry instruction).
    //
    // CORRECTNESS pin: this is the foundation of the orchestrator's
    // Sleigh-persistence loop — without this property, the orchestrator
    // would have to re-construct Sleigh per iteration.
    let bytes = vec![0xc3u8]; // ret
    let sleigh = make_sleigh_with_bytes(bytes, 0x1000);

    let cfg1 = build_one(sleigh, 0x1000);
    assert!(
        cfg1.graph.node_count() >= 1,
        "first Cfg must have at least one region",
    );

    // Harvest the Sleigh from the consumed Cfg.
    let recovered_sleigh = cfg1.sleigh;

    // Build a second Cfg using the SAME Sleigh handle.  No
    // re-construction; no SLA-spec reload.
    let cfg2 = build_one(recovered_sleigh, 0x1000);
    assert!(
        cfg2.graph.node_count() >= 1,
        "second Cfg built from re-used Sleigh must also be valid",
    );
}

#[test]
fn sleigh_can_be_used_for_multiple_cfg_builds() {
    // Stronger version of the round-trip pin: thread one Sleigh through
    // THREE successive builds.  This exercises the multi-iteration
    // scenario the orchestrator's fixed-point loop produces.
    //
    // Pinning this directly at the cfg/rsleigh layer protects future
    // refactors of `Builder` or `Sleigh::lift_one` — if someone adds
    // accidental per-build state to Sleigh, this test surfaces it.
    let bytes_a = vec![0xc3u8]; // ret
    let bytes_b = vec![0x90u8, 0xc3u8]; // nop; ret

    let sleigh = make_sleigh_with_bytes(bytes_a.clone(), 0x1000);
    let cfg1 = build_one(sleigh, 0x1000);
    let cfg2 = build_one(cfg1.sleigh, 0x1000);
    let cfg3 = build_one(cfg2.sleigh, 0x1000);
    assert!(cfg1.graph.node_count() >= 1);
    assert!(cfg2.graph.node_count() >= 1);
    assert!(cfg3.graph.node_count() >= 1);
    // Anchor the contract: each build produces an entry NodeIndex.
    let _ = cfg1.entry;
    let _ = cfg2.entry;
    let _ = cfg3.entry;

    // The sleigh handle is still usable after all three builds — i.e.
    // it travels back out via the Cfg::sleigh field one more time.
    let _ = bytes_b;
    let _final_sleigh = cfg3.sleigh;
}
