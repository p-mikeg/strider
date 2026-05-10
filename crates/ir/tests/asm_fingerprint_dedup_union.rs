//! Black-box: when the dedup cache returns the same `NodeId` for
//! semantically-equal `create_node` calls made under different
//! `lift_addr` values, the surviving node's asm-fingerprint must be
//! the **union** of both contributors — never the latest, never just
//! one.
//!
//! Pins the superset-only contract documented in
//! `docs/superpowers/specs/2026-05-03-asm-fingerprints-design.md` and
//! summarised in CLAUDE.md (`asm-fingerprint side-table`).
//!
//! Concrete failure mode caught: a future regression where the cache
//! hit returns the existing node *without* unioning the new
//! contributor's address — e.g. accidentally using
//! `set_asm_fingerprint` (overwrite) instead of
//! `extend_asm_fingerprint` (union).

#![allow(clippy::unwrap_used)]

use ir::FunctionBuilder;
use ir::node::NodeOutputType;

#[test]
fn dedup_unions_lift_addrs_across_multiple_contributors() {
    let mut b = FunctionBuilder::empty().unwrap();

    // Contributor #1 at lift_addr 0x1000.
    b.set_lift_addr(Some(0x1000));
    let out1 = b.build_int_const(42u64, NodeOutputType::U32).unwrap();
    b.set_lift_addr(None);

    // Contributor #2 at lift_addr 0x2000 — same value & type, must
    // dedup to the same NodeId via the dedup cache.
    b.set_lift_addr(Some(0x2000));
    let out2 = b.build_int_const(42u64, NodeOutputType::U32).unwrap();
    b.set_lift_addr(None);

    assert_eq!(out1, out2, "IntConst(42, U32) must dedup to one node");

    let g = b.graph_mut();
    let node = g.get_node_from_output(out1);
    let fingerprint = g.asm_fingerprint(node);
    assert_eq!(
        fingerprint,
        &[0x1000u64, 0x2000][..],
        "dedup must UNION both contributors' lift_addrs, got {fingerprint:?}",
    );
}

#[test]
fn three_consecutive_contributors_all_unioned() {
    let mut b = FunctionBuilder::empty().unwrap();
    let mut last_out = None;
    for addr in [0x1000u64, 0x2000, 0x3000] {
        b.set_lift_addr(Some(addr));
        last_out = Some(b.build_int_const(7u64, NodeOutputType::U16).unwrap());
        b.set_lift_addr(None);
    }
    let out = last_out.unwrap();
    let g = b.graph_mut();
    let node = g.get_node_from_output(out);
    let fingerprint = g.asm_fingerprint(node);
    assert_eq!(fingerprint, &[0x1000u64, 0x2000, 0x3000][..]);
}

#[test]
fn cache_hit_preserves_existing_addrs_when_no_lift_addr_set() {
    // Set lift_addr for the first call only; the second call without
    // a lift_addr should NOT shrink the existing fingerprint.
    let mut b = FunctionBuilder::empty().unwrap();

    b.set_lift_addr(Some(0x1000));
    let out1 = b.build_int_const(99u64, NodeOutputType::U64).unwrap();
    b.set_lift_addr(None);

    // Second call with no lift_addr — extend_asm_fingerprint(empty) is a no-op.
    let _ = b.build_int_const(99u64, NodeOutputType::U64).unwrap();

    let g = b.graph_mut();
    let node = g.get_node_from_output(out1);
    let fingerprint = g.asm_fingerprint(node);
    assert_eq!(
        fingerprint,
        &[0x1000u64][..],
        "no-lift-addr second contributor must not shrink the fingerprint",
    );
}
