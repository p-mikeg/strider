#![allow(clippy::unwrap_used, clippy::expect_used)]

//! End-to-end tests for `Builder::build` driven by small hand-crafted
//! x86-64 byte sequences. Covers scenarios that real binaries don't
//! exercise cleanly: region splitting by back-jump, `fn_max_size`
//! tail-call classification, and `allow_code_before_start_addr`.

mod common;
use common::{make_sleigh_with_bytes, TestReader};

use cfg::{Builder, OptionsBuilder, RegionEdgeKind};
use cfg::test_api::Options;
use petgraph::visit::IntoEdgeReferences;

fn build_from_bytes(bytes: Vec<u8>, start: u64) -> cfg::Cfg<TestReader> {
    Builder::new(
        make_sleigh_with_bytes(bytes, start),
        start,
        OptionsBuilder::new().build(),
    )
    .build()
    .expect("Builder::build on synthetic bytes")
}

fn build_from_bytes_opts(
    bytes: Vec<u8>,
    start: u64,
    opts: Options,
) -> cfg::Cfg<TestReader> {
    Builder::new(make_sleigh_with_bytes(bytes, start), start, opts)
        .build()
        .expect("Builder::build on synthetic bytes")
}

#[test]
fn single_ret_produces_one_region_without_tail_call_flag() {
    // `ret` at 0x1000 — single-region, non-tail-call function.
    let cfg = build_from_bytes(vec![0xc3], 0x1000);
    assert_eq!(cfg.graph.node_count(), 1);
    assert!(!cfg.graph[cfg.entry].ends_with_tail_call);
}

#[test]
fn back_jump_splits_region() {
    // At 0x1000: `xor eax, eax` (0x31 0xc0) — 2 bytes, non-terminating.
    // At 0x1002: `xor eax, eax` (0x31 0xc0) — 2 bytes, non-terminating.
    // At 0x1004: `jmp -4` (0xeb 0xfc) — jumps back to 0x1002 (mid-region).
    // The jump target 0x1002 is inside the already-decoded region, so
    // `explore` triggers `split_region`.
    //
    // Expected structure:
    //   region A: 0x1000..0x1002
    //   region B: 0x1002..0x1006 (jmp is last insn; back-edge to B's own start)
    // Edges: A -> B (Fallthrough), B -> B (Branch, the back-edge).
    let bytes = vec![0x31, 0xc0, 0x31, 0xc0, 0xeb, 0xfc];
    let cfg = build_from_bytes(bytes, 0x1000);

    assert!(
        cfg.graph.node_count() >= 2,
        "expected at least 2 regions after back-jump split; got {}",
        cfg.graph.node_count()
    );

    let branch_edges = cfg
        .graph
        .edge_references()
        .filter(|e| *e.weight() == RegionEdgeKind::Branch)
        .count();
    assert!(
        branch_edges >= 1,
        "expected at least one Branch edge from the back-jump"
    );
}

#[test]
fn fn_max_size_forces_forward_jump_to_be_tail_call() {
    // At 0x1000: `jmp +0x10` (0xeb 0x10) — target 0x1012.
    // With fn_max_size = 0x10, the target 0x1012 >= 0x1000 + 0x10 → tail call.
    // The jmp is the only pcode-terminator, so the function ends right there.
    let bytes = vec![0xeb, 0x10];
    let opts = OptionsBuilder::new().set_function_max_size(0x10).build();
    let cfg = build_from_bytes_opts(bytes, 0x1000, opts);
    assert_eq!(cfg.graph.node_count(), 1);
    assert!(
        cfg.graph[cfg.entry].ends_with_tail_call,
        "entry region must be flagged as ending in a tail call"
    );
}

#[test]
fn allow_code_before_start_addr_negates_below_start_tail_call() {
    // Place a valid `ret` below the function start at 0x0ff2.
    // Then at 0x1000: `jmp -16` (0xeb 0xf0) → target 0x0ff2.
    //
    // Without `allow_code_before_start_addr`, the jmp is classified as a
    // tail call. With the option set, it must be followed normally, producing
    // a Branch edge and (at least) 2 regions — entry and the target.
    let mut bytes = vec![0u8; 0x14]; // spans 0x0ff0..0x1004
    bytes[0x02] = 0xc3;              // 0x0ff2: ret
    bytes[0x10] = 0xeb;              // 0x1000: jmp
    bytes[0x11] = 0xf0;              // rel8 = -16 → target 0x0ff2

    let sleigh = rsleigh::Sleigh::new(
        rsleigh::sla_spec::SLA_SPEC_X86_64,
        rsleigh::pspec::PSPEC_X86_64,
        rsleigh::mem_readers::BufMemReader::new(bytes, 0x0ff0),
    ).unwrap();

    let opts = OptionsBuilder::new().allow_code_before_start_addr().build();
    let cfg = Builder::new(sleigh, 0x1000, opts).build().unwrap();

    // Entry region must NOT be flagged as ending in a tail call.
    assert!(!cfg.graph[cfg.entry].ends_with_tail_call);

    // At least one Branch edge must exist, since the target is now followed.
    assert!(
        cfg.graph
            .edge_references()
            .any(|e| *e.weight() == RegionEdgeKind::Branch),
        "expected at least one Branch edge since the below-start target is followed"
    );
}
