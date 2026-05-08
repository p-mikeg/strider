#![allow(clippy::unwrap_used, clippy::expect_used)]

//! End-to-end tests for `Builder::build` driven by small hand-crafted
//! x86-64 byte sequences. Covers scenarios that real binaries don't
//! exercise cleanly: region splitting by back-jump, `fn_max_size`
//! tail-call classification, and `allow_code_before_start_addr`.

mod common;
use common::{make_sleigh_with_bytes, TestReader};

use cfg::{Builder, OptionsBuilder, RegionEdgeKind, RegionTerminator};
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
    assert_eq!(cfg.graph[cfg.entry].terminator, RegionTerminator::Return);
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
    assert_eq!(
        cfg.graph[cfg.entry].terminator,
        RegionTerminator::TailCall { target: 0x1012 },
        "entry region must end as a TailCall to 0x1012"
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
    assert_ne!(
        cfg.graph[cfg.entry].terminator,
        RegionTerminator::TailCall { target: 0x0ff2 },
        "entry region must NOT be a TailCall when allow_code_before_start_addr is set"
    );

    // At least one Branch edge must exist, since the target is now followed.
    assert!(
        cfg.graph
            .edge_references()
            .any(|e| *e.weight() == RegionEdgeKind::Branch),
        "expected at least one Branch edge since the below-start target is followed"
    );
}

/// Regression: a function whose body has no explicit terminator within
/// `fn_max_size` (e.g. ends with `call <noreturn>` followed by padding)
/// must terminate the lifted region cleanly at the bound rather than
/// fall-through-decoding into the next symbol.
///
/// The original bug: fall-through past `start + fn_max_size` would lift
/// arbitrary OOB instructions.  When one of those happened to be a
/// multi-pcode-op instruction (e.g. `lock cmpxchg` with intra-insn
/// CONST branches), `decode_branch_target`'s CONST arm produced a
/// `PcodeInsnAddr { machine_addr: <OOB>, insn_index: <nonzero> }`,
/// which the `Branch` / `CondBranch` arms' inlined `insn_index == 0`
/// validation rejected with "invalid tail call at opcode ...".
///
/// Fix: at the top of every `RegionBuilder::build()` iteration, if the
/// (already-advanced) `cur_addr` is past `start + fn_max_size`, finish
/// the region with `TailCall { target: cur_addr.machine_addr }`.
/// Companion to `fall_through_past_fn_max_size_terminates_as_tail_call`:
/// when a `CondBranch` (jcc) sits exactly at the function's upper
/// bound, its fall-through `next_insn_addr` lands past `start +
/// fn_max_size`.  Pre-fix, the cfg builder enqueued the OOB
/// fall-through address as a normal work-queue item, lifting whatever
/// machine bytes lived there.  Post-fix the builder pre-classifies
/// both `CondBranch` targets and drops the OOB edge — when only the
/// fall-through is OOB, the region's terminator becomes `Branch` to
/// the in-range taken target (the conditional collapses but the lift
/// proceeds).
#[test]
fn cond_branch_with_oob_fallthrough_collapses_to_branch_in_range() {
    // 0x1000: `xor eax, eax`        (2 bytes, sets ZF)
    // 0x1002: `je 0x1000`           (2 bytes, taken target = 0x1000, in-range;
    //                                fall-through = 0x1004, OOB at fn_max_size=4)
    let mut bytes = vec![0x31u8, 0xc0, 0x74, 0xfc];
    // Pad so any over-read past the bound (orchestrator decode probes,
    // pre-classification lifts, etc.) finds valid memory.
    bytes.extend(std::iter::repeat_n(0x90u8, 64));
    let opts = OptionsBuilder::new().set_function_max_size(4).build();
    let cfg = build_from_bytes_opts(bytes, 0x1000, opts);

    // The entry region terminator must NOT be CondBranch (the OOB
    // fall-through would have crashed the IR layer's `handle_cond_branch`,
    // which requires both successors to exist).
    assert!(
        !matches!(cfg.graph[cfg.entry].terminator, RegionTerminator::CondBranch),
        "entry region must not retain CondBranch when one successor is OOB"
    );
}

/// Both `CondBranch` targets OOB: the function leaves either way, so
/// the region collapses to `TailCall { target: <taken_target> }`.
#[test]
fn cond_branch_with_both_targets_oob_collapses_to_tail_call() {
    // 0x1000: `xor eax, eax`             (2 bytes)
    // 0x1002: `je 0x1100`                (6 bytes for `0F 84 rel32`; both
    //                                     taken (0x1100) and fall-through
    //                                     (0x1008) are OOB at fn_max_size=2)
    // The bound is `2`, so even the second instruction is OOB — we'll
    // never reach the jcc.  Use a smaller setup: place jcc as the
    // first instruction within fn_max_size that still has both
    // targets OOB.  fn_max_size=6 → end=0x1006.  Taken target via
    // rel8: `74 7E` → +0x7e → 0x1080 (OOB).  Fall-through 0x1002 (in!).
    // So we need both rel target AND fall-through to be OOB.  Use:
    //   xor eax,eax (2 bytes) at 0x1000
    //   je   0x108? (rel8 = +0x7e, 2 bytes) at 0x1002 → taken=0x1082 OOB
    //   nop         (filler) at 0x1004
    // fn_max_size=4 with rel8 puts taken OOB but fall-through (0x1004)
    // in-range.  We need a *single*-instruction setup where the
    // CondBranch is the FIRST insn and BOTH its outcomes are OOB.
    //
    // x86 jcc rel8 is 2 bytes.  Place it at 0x1000 with fn_max_size=2:
    //   0x1000: 74 7E      → je 0x1080 (taken OOB)
    //   fall-through 0x1002 also OOB.
    //
    // This requires entering the region's CondBranch arm before the
    // `cur_addr` bound check fires — `RegionBuilder::build` lifts and
    // processes the jcc on the very first iteration.
    let mut bytes = vec![0x74u8, 0x7e];
    bytes.extend(std::iter::repeat_n(0x90u8, 256));
    let opts = OptionsBuilder::new().set_function_max_size(2).build();
    let cfg = build_from_bytes_opts(bytes, 0x1000, opts);

    // Both successors OOB → terminator is TailCall (the function
    // leaves either way).  The exact target (taken vs fall-through)
    // doesn't carry observable semantics; pin the kind only.
    assert!(
        matches!(cfg.graph[cfg.entry].terminator, RegionTerminator::TailCall { .. }),
        "entry region must collapse to TailCall when both CondBranch successors are OOB; got {:?}",
        cfg.graph[cfg.entry].terminator
    );
}

#[test]
fn fall_through_past_fn_max_size_terminates_as_tail_call() {
    // 0x1000: `xor eax, eax` (2 bytes, ≥1 pcode insn — appends to the
    //         region's `insns` list).
    // 0x1002: `lock cmpxchg %r14, 0x58(%rbx)` (6 bytes) — multi-pcode-op
    //         insn with intra-insn CONST branches; lifting it past the
    //         bound was the exact crash shape from `dounmount`.
    let mut bytes = vec![0x31u8, 0xc0];
    bytes.extend_from_slice(&[0xF0, 0x4C, 0x0F, 0xB1, 0x73, 0x58]);
    bytes.extend(std::iter::repeat_n(0x90u8, 16));
    let opts = OptionsBuilder::new().set_function_max_size(2).build();
    let cfg = build_from_bytes_opts(bytes, 0x1000, opts);

    // Entry region must terminate as TailCall { target: 0x1002 } — the
    // first OOB byte after the bound.
    assert_eq!(
        cfg.graph[cfg.entry].terminator,
        RegionTerminator::TailCall { target: 0x1002 },
        "fall-through past fn_max_size must terminate as TailCall to the OOB byte"
    );
}
