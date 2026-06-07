//! `build_switch_if_ladder` integration tests for jump-table (`Switch`) lifting.
//!
//! Drives the full `build_ir` → `handle_switch` →
//! `build_switch_if_ladder` path with a real x86-64 BranchIndirect
//! that's resolved to `Multiple([t0, t1, ...])` via the cfg
//! builder's `LiftOptions::known_targets` feedback path — the same path
//! the strider fixed-point orchestrator uses to commit a IR-level
//! `Multiple` classification across iterations.
//!
//! Each test constructs a tiny x86-64 byte sequence whose control
//! flow is `jmp rax` followed by N short target regions (each one
//! `ret`), feeds the BranchIndirect's pcode address into
//! `LiftOptions::known_targets` with a `Multiple` payload pointing at those
//! targets, runs `build_ir`, and asserts on the resulting IR
//! shape.
//!
//! The unit-level coverage of `build_switch_if_ladder`'s primitive
//! lives in `crates/strider-lift/src/lift/insn/control.rs::tests`.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use strider_ir::{IRViewer, IRWalker};
use rsleigh::mem_readers::BufMemReader;
use rustc_hash::FxHashMap;
use strider_ir::Function;
use strider_ir::node::{IntPayload, NodeKind};
use strider_cfg::{MachineInsnAddr, PcodeInsnAddr, ResolvedTargets};

mod common;

fn count_eq_cmps(function: &Function) -> usize {
    function.count_kind(|k| matches!(k, NodeKind::IntCmpOp(strider_ir::IntCmpOp::Equal)))
}

fn count_int_consts_eq(function: &Function, want: u64) -> usize {
    function.count_kind(|k| matches!(k, NodeKind::IntConst(IntPayload::Small(c)) if *c == want))
}

#[test]
fn switch_terminator_lifts_to_if_ladder_for_one_target() {
    // 1-target Switch — degenerate ladder collapses to a plain
    // `build_branch`.  No If, no IntCmpOp, no comparison constant
    // for the dispatch value.  Pinning this shape ensures the
    // single-target case doesn't regress into a 1-arm If with a
    // dead default.
    let (bytes, base, ba, targets) = common::synth_jmp_rax_with_targets(1);
    let (g, _, _) = common::analyze_with_known_targets(&bytes, base, ba, &targets);
    assert_eq!(common::count_ifs(&g), 0, "no If for 1-target Switch");
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
    let (bytes, base, ba, targets) = common::synth_jmp_rax_with_targets(3);
    let (g, _, _) = common::analyze_with_known_targets(&bytes, base, ba, &targets);
    assert_eq!(
        common::count_ifs(&g),
        2,
        "N-1=2 If nodes for 3-target Switch"
    );
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
    // known_targets.  After build_ir + the default pipeline
    // (which includes ConstantFold + PhiCollapse +
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
        0x48, 0xc7, 0xc0, pick_le[0], pick_le[1], pick_le[2], pick_le[3], 0xff, 0xe0, 0xc3, 0xc3,
        0xc3,
    ];
    bytes.extend(std::iter::repeat_n(0xccu8, 16));
    let branch_indirect_addr = 0x1007u64; // jmp rax sits right after the mov

    let reader = BufMemReader::new(bytes, base);
    // The driver OWNS the Sleigh and builds the CFG itself.
    let (mut strider, cc) = common::strider_x86_64(reader);
    let mut known_targets: FxHashMap<PcodeInsnAddr, ResolvedTargets> = FxHashMap::default();
    known_targets.insert(
        PcodeInsnAddr {
            machine_addr: MachineInsnAddr::from(branch_indirect_addr),
            insn_index: 0,
        },
        ResolvedTargets::Multiple(target_addrs.clone()),
    );
    let cfg_opts = strider_cfg::CfgOptions {
        known_targets,
        ..Default::default()
    };
    let cfg = strider
        .build_cfg(MachineInsnAddr::from(base), &cfg_opts)
        .expect("cfg build");

    let outcome = strider.build_ir(&cfg, &cc).expect("build_ir");
    let mut function = outcome.function;

    // Sanity: pre-optimization, the `build_switch_if_ladder` if-ladder produced N-1 = 2
    // If nodes.  After the default pipeline collapses the
    // constant-index dispatch, all of them should be gone
    // (DeadBranchElimination removes If nodes whose conditions are
    // BoolConst).
    let if_count_pre = common::count_ifs(&function);
    assert!(
        if_count_pre >= 2,
        "expected at least 2 If nodes pre-optimization, got {if_count_pre}",
    );

    let pipeline = strider_orchestrator::opt::default_pipeline();
    pipeline
        .run(&mut function, &mut strider_orchestrator::opt::OptCtx::empty())
        .expect("optimizer pipeline");

    let if_count_post = common::count_ifs(&function);
    assert_eq!(
        if_count_post, 0,
        "constant-index Switch must collapse to zero If nodes after \
         ConstantFold + DeadBranchElimination; got {if_count_post}",
    );
}

#[test]
fn switch_targets_are_not_double_linked_by_the_region_linker() {
    // Regression: a `Switch` region's per-target CFG edges carry the
    // `Unconditional` edge kind, but the region's *IR* control flow is
    // wired exclusively by `handle_switch`'s If-ladder (`build_if` /
    // `build_branch`).  The post-loop `link_region_edges` linker MUST
    // therefore skip a `Switch` region's `Unconditional` edges — re-linking
    // them adds the switch region's pre-If control as a spurious second
    // predecessor to every target region.  The structural validator can't
    // catch it on its own because the target's `Region` fan-in and `MemPhi`
    // arity inflate in lock-step (both grow by one per spurious link).
    //
    // The synthetic fixture (`jmp rax` → N single-`ret` targets) has no
    // merging control flow, so a correctly-lifted graph is a pure control
    // tree: every `Region` node has at most one control predecessor.  The
    // double-link gave each of the N target regions two.
    for n in [1usize, 2, 3] {
        let (bytes, base, ba, targets) = common::synth_jmp_rax_with_targets(n);
        let (g, _, _) = common::analyze_with_known_targets(&bytes, base, ba, &targets);
        let over_linked: Vec<_> = g
            .walk()
            .filter(|&nid| matches!(g.node_kind(nid), NodeKind::Region))
            .filter(|&nid| g.node_inputs(nid).len() >= 2)
            .collect();
        assert!(
            over_linked.is_empty(),
            "{n}-target switch: tree-shaped control flow must have no merge \
             regions, but these Region nodes carry >=2 control predecessors \
             (link_region_edges double-linked the Switch region's \
             Unconditional edges that handle_switch already wired): \
             {over_linked:?}",
        );
    }
}

#[test]
fn ir_level_multiple_resolution_end_to_end_produces_lifted_switch_in_ir() {
    // End-to-end pin: a CFG that has a `BranchIndirect` resolved
    // to `Multiple([t0, t1])` via `with_known_targets` produces an
    // IR graph containing the `build_switch_if_ladder` If-ladder corresponding to those
    // targets.  Verifies the full
    // `build_ir → handle_switch → build_switch_if_ladder`
    // pipeline produces visible IR structure that downstream
    // consumers (pattern queries, dot rendering) can pattern-match
    // against — closing the gap that pre-`build_switch_if_ladder` produced CFG edges
    // with no IR encoding for the dispatch.
    let (bytes, base, ba, targets) = common::synth_jmp_rax_with_targets(2);
    let (g, _, _) = common::analyze_with_known_targets(&bytes, base, ba, &targets);
    // 2-target Switch: exactly one If, one equality cmp.
    assert_eq!(common::count_ifs(&g), 1, "2-target Switch produces one If");
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
        .walk()
        .filter(|nid| matches!(g.node_kind(*nid), NodeKind::IndirectBranch))
        .count();
    assert_eq!(
        placeholder_count, 0,
        "IR-level `Multiple` resolution must NOT leave an IndirectBranch placeholder",
    );
}
