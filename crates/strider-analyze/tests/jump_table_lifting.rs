//! `build_switch_if_ladder` integration tests for jump-table (`Switch`) lifting.
//!
//! Drives the full `analyze_cfg` → `handle_switch` →
//! `build_switch_if_ladder` path with a real x86-64 BranchIndirect
//! that's resolved to `Multiple([t0, t1, ...])` via the cfg
//! builder's `with_known_targets` feedback path — the same path
//! the strider fixed-point orchestrator uses to commit a IR-level
//! `Multiple` classification across iterations.
//!
//! Each test constructs a tiny x86-64 byte sequence whose control
//! flow is `jmp rax` followed by N short target regions (each one
//! `ret`), feeds the BranchIndirect's pcode address into
//! `with_known_targets` with a `Multiple` payload pointing at those
//! targets, runs `analyze_cfg`, and asserts on the resulting IR
//! shape.
//!
//! The unit-level coverage of `build_switch_if_ladder`'s primitive
//! lives in `crates/strider-analyze/src/strider/insn/control.rs::tests`.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::HashMap;

use strider_lift::cfg::{
    Builder, MachineInsnAddr, OptionsBuilder, PcodeInsnAddr, ResolvedTargets,
};
use strider_ir::node::NodeKind;
use strider_ir::Graph;
use rsleigh::Sleigh;
use rsleigh::mem_readers::BufMemReader;
use strider_target::SleighArch;

mod common;

/// Build a synthetic x86-64 binary whose entry is `jmp rax` (a
/// BranchIndirect) followed by `n_targets` 1-byte `ret` regions
/// laid out contiguously starting at `0x1002`.
///
/// Returns `(bytes, base, branch_indirect_addr, target_addrs)`.
fn synth_jmp_rax_with_targets(n_targets: usize) -> (Vec<u8>, u64, u64, Vec<u64>) {
    let base = 0x1000u64;
    let mut bytes = vec![0xffu8, 0xe0]; // jmp rax — 2 bytes at 0x1000
    let mut target_addrs = Vec::with_capacity(n_targets);
    for i in 0..n_targets {
        let target_addr = base + 2 + i as u64; // 0x1002, 0x1003, ...
        target_addrs.push(target_addr);
        bytes.push(0xc3); // ret
    }
    // Pad with int3 so any speculative look-ahead past the last
    // ret doesn't fault the BufMemReader.  16 bytes is overkill
    // for the tests but matches the fixture pattern in indirect_resolve_helpers.
    bytes.extend(std::iter::repeat_n(0xccu8, 16));
    let branch_indirect_addr = base; // first pcode insn at the entry
    (bytes, base, branch_indirect_addr, target_addrs)
}

/// Run `analyze_cfg` on a hand-assembled x86-64 byte sequence with
/// `known_targets` pre-seeded for the BranchIndirect at
/// `branch_indirect_addr`.  Returns the resulting graph.
fn analyze_with_known_targets(
    bytes: Vec<u8>,
    base: u64,
    branch_indirect_addr: u64,
    targets: Vec<u64>,
) -> Graph {
    let arch = SleighArch::x86_64();
    let reader = BufMemReader::new(bytes, base);
    let sleigh =
        Sleigh::new(arch.sla_spec(), arch.pspec(), reader).expect("create x86_64 sleigh");
    // Seed `known_targets` so the cfg builder produces
    // `RegionTerminator::Switch` (not `UnresolvedIndirectBranch`)
    // for the BranchIndirect at `branch_indirect_addr`.
    let mut known_targets: HashMap<PcodeInsnAddr, ResolvedTargets> = HashMap::new();
    let key = PcodeInsnAddr { machine_addr: MachineInsnAddr::from(branch_indirect_addr), insn_index: 0 };
    known_targets.insert(key, ResolvedTargets::Multiple(targets));
    let opts = OptionsBuilder::new().build();
    let cfg = Builder::for_arch(&arch, sleigh, base, opts)
        .with_known_targets(known_targets)
        .build()
        .expect("cfg build with Multiple known target");

    let strider = common::strider_x86_64();
    strider.analyze_cfg(&cfg).expect("analyze_cfg").graph
}

fn count_if_nodes(g: &Graph) -> usize {
    g.count_kind(|k| matches!(k, NodeKind::If))
}

fn count_eq_cmps(g: &Graph) -> usize {
    g.count_kind(|k| matches!(k, NodeKind::IntCmpOp(strider_ir::IntCmpOp::Equal)))
}

fn count_int_consts_eq(g: &Graph, want: u64) -> usize {
    g.count_kind(|k| matches!(k, NodeKind::IntConst(c) if *c == u128::from(want)))
}

#[test]
fn switch_terminator_lifts_to_if_ladder_for_one_target() {
    // 1-target Switch — degenerate ladder collapses to a plain
    // `build_branch`.  No If, no IntCmpOp, no comparison constant
    // for the dispatch value.  Pinning this shape ensures the
    // single-target case doesn't regress into a 1-arm If with a
    // dead default.
    let (bytes, base, ba, targets) = synth_jmp_rax_with_targets(1);
    let g = analyze_with_known_targets(bytes, base, ba, targets.clone());
    assert_eq!(count_if_nodes(&g), 0, "no If for 1-target Switch");
    assert_eq!(count_eq_cmps(&g), 0, "no equality cmp for 1-target Switch");
    // Still no comparison-constant for K_0 — `build_switch_if_ladder` emits the const
    // ONLY when there's a cmp.
    assert_eq!(
        count_int_consts_eq(&g, targets[0]),
        0,
        "1-target Switch must not emit a K_0 comparison constant",
    );
}

#[test]
fn switch_terminator_lifts_to_if_ladder_for_three_targets() {
    // 3-target Switch — exactly N-1 = 2 If nodes and 2 IntCmpOp
    // equality cmps.  The K_0 and K_1 comparison constants must
    // appear; K_{N-1} (= K_2) must NOT — its region is reached
    // via the last If's false-branch.  Pinning the polarity here
    // catches any future regression that flips true/false sides
    // or that introduces a redundant final cmp.
    let (bytes, base, ba, targets) = synth_jmp_rax_with_targets(3);
    let g = analyze_with_known_targets(bytes, base, ba, targets.clone());
    assert_eq!(count_if_nodes(&g), 2, "N-1=2 If nodes for 3-target Switch");
    assert_eq!(
        count_eq_cmps(&g),
        2,
        "N-1=2 equality cmps for 3-target Switch",
    );
    assert!(
        count_int_consts_eq(&g, targets[0]) >= 1,
        "K_0 comparison constant present",
    );
    assert!(
        count_int_consts_eq(&g, targets[1]) >= 1,
        "K_1 comparison constant present",
    );
    assert_eq!(
        count_int_consts_eq(&g, targets[2]),
        0,
        "K_{{N-1}} (final target) NOT compared — flows via last If's false-branch",
    );
}

#[test]
fn switch_with_const_index_collapses_via_default_pipeline_to_single_branch() {
    // `build_switch_if_ladder` + ConstantFold composition: `build_switch_if_ladder`'s lifted If-ladder uses
    // the existing `IntCmpOp::Equal` + `If` primitives, so when
    // the index folds to a constant the existing `ConstantFold` +
    // `DeadBranchElimination` passes prune the dead arms and
    // collapse the dispatch to a single Branch.
    //
    // Synthetic shape: `mov rax, K_target; jmp rax` where
    // K_target is one of the Multiple targets we feed via
    // known_targets.  After analyze_cfg + the default pipeline
    // (which includes ConstantFold + RedundantPhis +
    // DeadBranchElimination), only the matching K's branch
    // survives — pinned by the `If` count dropping to 0.
    //
    // x86-64 encoding:
    //   48 c7 c0 LL LL LL LL   mov rax, imm32 (sign-extended; 7 bytes)
    //   ff e0                  jmp rax        (2 bytes)
    //   c3 c3 c3               three target rets (1 byte each)
    let base = 0x1000u64;
    let target_addrs = vec![0x100au64, 0x100bu64, 0x100cu64];
    let pick = target_addrs[1]; // pick K_1
    let pick_le = (pick as u32).to_le_bytes();
    let mut bytes: Vec<u8> = vec![
        0x48,
        0xc7,
        0xc0,
        pick_le[0],
        pick_le[1],
        pick_le[2],
        pick_le[3],
        0xff,
        0xe0,
        0xc3,
        0xc3,
        0xc3,
    ];
    bytes.extend(std::iter::repeat_n(0xccu8, 16));
    let branch_indirect_addr = 0x1007u64; // jmp rax sits right after the mov

    let arch = SleighArch::x86_64();
    let reader = BufMemReader::new(bytes, base);
    let sleigh =
        Sleigh::new(arch.sla_spec(), arch.pspec(), reader).expect("create x86_64 sleigh");
    let mut known_targets: HashMap<PcodeInsnAddr, ResolvedTargets> = HashMap::new();
    known_targets.insert(
        PcodeInsnAddr { machine_addr: MachineInsnAddr::from(branch_indirect_addr), insn_index: 0 },
        ResolvedTargets::Multiple(target_addrs.clone()),
    );
    let opts = OptionsBuilder::new().build();
    let cfg = Builder::for_arch(&arch, sleigh, base, opts)
        .with_known_targets(known_targets)
        .build()
        .expect("cfg build");

    let strider = common::strider_x86_64();
    let outcome = strider.analyze_cfg(&cfg).expect("analyze_cfg");
    let mut graph = outcome.graph;

    // Sanity: pre-optimization, the `build_switch_if_ladder` if-ladder produced N-1 = 2
    // If nodes.  After the default pipeline collapses the
    // constant-index dispatch, all of them should be gone
    // (DeadBranchElimination removes If nodes whose conditions are
    // BoolConst).
    let if_count_pre = count_if_nodes(&graph);
    assert!(
        if_count_pre >= 2,
        "expected at least 2 If nodes pre-optimization, got {if_count_pre}",
    );

    let pipeline = strider.build_optimizer_pipeline();
    let entry = graph.entry().unwrap();
    pipeline
        .run(graph.graph_mut(), entry)
        .expect("optimizer pipeline");

    let if_count_post = count_if_nodes(&graph);
    assert_eq!(
        if_count_post, 0,
        "constant-index Switch must collapse to zero If nodes after \
         ConstantFold + DeadBranchElimination; got {if_count_post}",
    );
}

#[test]
fn ir_level_multiple_resolution_end_to_end_produces_lifted_switch_in_ir() {
    // End-to-end pin: a CFG that has a `BranchIndirect` resolved
    // to `Multiple([t0, t1])` via `with_known_targets` produces an
    // IR graph containing the `build_switch_if_ladder` If-ladder corresponding to those
    // targets.  Verifies the full
    // `analyze_cfg → handle_switch → build_switch_if_ladder`
    // pipeline produces visible IR structure that downstream
    // consumers (pattern queries, dot rendering) can pattern-match
    // against — closing the gap that pre-`build_switch_if_ladder` produced CFG edges
    // with no IR encoding for the dispatch.
    let (bytes, base, ba, targets) = synth_jmp_rax_with_targets(2);
    let g = analyze_with_known_targets(bytes, base, ba, targets.clone());
    // 2-target Switch: exactly one If, one equality cmp.
    assert_eq!(count_if_nodes(&g), 1, "2-target Switch produces one If");
    assert_eq!(count_eq_cmps(&g), 1, "2-target Switch produces one cmp");
    // K_0 is the comparison constant; K_1 is the false-branch's
    // final target (no constant emitted for it).
    assert!(
        count_int_consts_eq(&g, targets[0]) >= 1,
        "K_0 comparison constant must appear in the lifted IR",
    );
    // No leftover IndirectBranch placeholder: the orchestrator's
    // unresolved-branch table should be empty here because the
    // BranchIndirect was fully classified to Multiple before lift
    // (no UnresolvedIndirectBranch placeholder generated).
    let placeholder_count = g
        .preorder()
        .filter(|nid| matches!(g.node_kind(*nid), NodeKind::IndirectBranch))
        .count();
    assert_eq!(
        placeholder_count, 0,
        "IR-level `Multiple` resolution must NOT leave an IndirectBranch placeholder",
    );
}
