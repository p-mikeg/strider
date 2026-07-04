//! `Switch`-node integration tests for jump-table lifting.
//!
//! Drives the full `build_ir` → `handle_switch` → `build_switch` path
//! with a real x86-64 BranchIndirect that's resolved to
//! `Multiple([t0, t1, ...])` via the cfg builder's
//! `LiftOptions::known_targets` feedback path — the same path the
//! strider fixed-point orchestrator uses to commit an IR-level
//! `Multiple` classification across iterations.
//!
//! Each test constructs a tiny x86-64 byte sequence whose control
//! flow is `jmp rax` followed by N short target regions (each one
//! `ret`), feeds the BranchIndirect's pcode address into
//! `LiftOptions::known_targets` with a `Multiple` payload pointing at those
//! targets, runs `build_ir`, and asserts on the resulting IR
//! shape: a single `NodeKind::Switch` with one `Control` output per
//! target (in target order), and zero `If` / `IntCmpOp::Equal` nodes
//! arising from the dispatch (the old if-ladder lowering —
//! `IntCmpOp::Equal` + `If` per arm — was replaced by `handle_switch`
//! emitting one `Switch` node directly; case addresses now live in the
//! `switch_targets` side table instead of as IR comparison constants).
//!
//! The unit-level coverage of `build_switch`'s primitive lives in
//! `crates/strider-lift/src/lift/control.rs::tests`.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use rsleigh::mem_readers::BufMemReader;
use rustc_hash::FxHashMap;
use strider_cfg::{MachineInsnAddr, PcodeInsnAddr, ResolvedTargets};
use strider_ir::node::NodeKind;
use strider_ir::{Function, IRViewer, IRWalker};
use strider_ir_test_utils::IrWalkerEx;

mod common;

fn count_eq_cmps(function: &Function) -> usize {
    function.count_kind(|k| matches!(k, NodeKind::IntCmpOp(strider_ir::IntCmpOp::Equal)))
}

fn count_switches(function: &Function) -> usize {
    function.count_kind(|k| matches!(k, NodeKind::Switch))
}

/// Locates the unique `Switch` node in `function`. Panics if zero or more
/// than one is present — either case indicates a fixture-construction bug.
fn find_unique_switch(function: &Function) -> strider_ir::node::NodeId {
    let mut iter = function
        .walk()
        .filter(|&nid| matches!(function.node_kind(nid), NodeKind::Switch));
    let first = iter
        .next()
        .expect("fixture must contain exactly one Switch node");
    assert!(
        iter.next().is_none(),
        "fixture has more than one Switch node"
    );
    first
}

fn count_int_consts_eq(function: &Function, want: u64) -> usize {
    function
        .walk()
        .filter(|&nid| {
            if !matches!(function.node_kind(nid), NodeKind::IntConst(_)) {
                return false;
            }
            function
                .first_value_output_of(nid)
                .is_some_and(|v| function.int_const_u128(v) == Some(u128::from(want)))
        })
        .count()
}

#[test]
fn switch_terminator_lifts_to_plain_branch_for_one_target() {
    // 1-target Switch — `handle_switch`'s degenerate case emits a plain
    // `build_branch`: no `Switch` node, no `If`, no `IntCmpOp`, and no
    // comparison constant for the dispatch value.  Pinning this shape
    // ensures the single-target case doesn't regress into a spurious
    // 1-arm `Switch` (or a 1-arm `If` with a dead default).
    let (bytes, base, ba, targets) = common::synth_jmp_rax_with_targets(1);
    let (g, _, _) = common::analyze_with_known_targets(&bytes, base, ba, &targets);
    assert_eq!(
        count_switches(&g),
        0,
        "no Switch node for 1-target dispatch"
    );
    assert_eq!(common::count_ifs(&g), 0, "no If for 1-target dispatch");
    assert_eq!(
        count_eq_cmps(&g),
        0,
        "no equality cmp for 1-target dispatch"
    );
    // Still no comparison-constant for K_0 — there is no cmp at all in
    // the plain-branch degenerate case.
    assert_eq!(
        count_int_consts_eq(&g, targets[0]),
        0,
        "1-target dispatch must not emit a K_0 comparison constant",
    );
}

#[test]
fn switch_terminator_lifts_to_single_switch_node_for_three_targets() {
    // 3-target Switch — `handle_switch` emits exactly one
    // `NodeKind::Switch` with N=3 `Control` outputs (one per target
    // region, in target order).  There is no if-ladder anymore, so
    // zero `If` nodes and zero `IntCmpOp::Equal` cmps.  The target
    // addresses live in the `switch_targets` side table (not as IR
    // comparison constants) — assert they match the fixture's targets,
    // in order.  Pinning this shape catches any future regression that
    // reintroduces a decomposed cmp/If ladder or scrambles target
    // order.
    let (bytes, base, ba, targets) = common::synth_jmp_rax_with_targets(3);
    let (g, _, _) = common::analyze_with_known_targets(&bytes, base, ba, &targets);
    assert_eq!(
        common::count_ifs(&g),
        0,
        "3-target Switch produces zero If nodes"
    );
    assert_eq!(
        count_eq_cmps(&g),
        0,
        "3-target Switch produces zero equality cmps",
    );
    assert_eq!(
        count_switches(&g),
        1,
        "exactly one Switch node for 3-target dispatch",
    );
    let switch_id = find_unique_switch(&g);
    assert_eq!(
        g.node_outputs(switch_id).len(),
        3,
        "Switch has one Control output per target",
    );
    assert_eq!(
        g.side_tables().switch_targets(switch_id),
        targets.as_slice(),
        "switch_targets side table must list the 3 target addresses in target order",
    );
}

#[test]
fn switch_with_const_index_collapses_through_default_pipeline() {
    // `handle_switch` + the default pipeline: a `Switch`'s `address`
    // input is just another value input, so when the dispatch value is a
    // compile-time constant (as here — `mov rax, K_target; jmp rax`),
    // `ConstantFold` reduces it to an `IntConst` and `DeadBranchElimination`
    // then collapses the constant-address `Switch` to its single matching
    // arm.  This test pins that contract: pre-optimization there is exactly
    // one `Switch` with 3 `Control` outputs; post-optimization the `Switch`
    // is gone (collapsed to case 1), and — since a `Switch` never decomposes
    // into an if-ladder — the `If` count is zero both before and after.
    //
    // Synthetic shape: `mov rax, K_target; jmp rax` where
    // K_target is one of the Multiple targets we feed via
    // known_targets.
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
        .build_cfg(MachineInsnAddr::from(base), &cfg_opts, &Default::default())
        .expect("cfg build");

    let outcome = strider.build_ir(&cfg, cc).expect("build_ir");
    let mut function = outcome.function;

    // Pre-optimization: exactly one Switch node, zero If nodes (a
    // Switch never decomposes into a cmp/If ladder).
    assert_eq!(
        count_switches(&function),
        1,
        "expected one Switch node pre-optimization",
    );
    assert_eq!(
        common::count_ifs(&function),
        0,
        "expected zero If nodes pre-optimization",
    );

    let pipeline = strider_orchestrator::opt::default_pipeline();
    pipeline
        .run(
            &mut function,
            &mut strider_orchestrator::opt::OptCtx::new(None),
        )
        .expect("optimizer pipeline");

    // Post-optimization: `ConstantFold` reduces the dispatch value to
    // `IntConst(K_1)` (== `target_addrs[1]`, i.e. case 1), so
    // `DeadBranchElimination` collapses the constant-address `Switch` to its
    // single matching arm — the `Switch` is killed and control flows straight
    // to case 1's region.  No `Switch` survives, and the If count stays zero.
    assert_eq!(
        count_switches(&function),
        0,
        "constant-index Switch collapses to its matching arm (DeadBranchElimination)",
    );
    assert_eq!(
        common::count_ifs(&function),
        0,
        "Switch-lowered dispatch never produces If nodes, pre- or post-optimization",
    );
}

#[test]
fn switch_targets_are_not_double_linked_by_the_region_linker() {
    // Regression: a `Switch` region's per-target CFG edges carry the
    // `Unconditional` edge kind, but the region's *IR* control flow is
    // wired exclusively by `handle_switch`'s dispatch (`build_switch` /
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
    // IR graph containing the `Switch` node corresponding to those
    // targets.  Verifies the full
    // `build_ir → handle_switch → build_switch`
    // pipeline produces visible IR structure that downstream
    // consumers (pattern queries, dot rendering) can pattern-match
    // against — closing the gap that pre-`Switch` CFG edges
    // had no IR encoding for the dispatch.
    let (bytes, base, ba, targets) = common::synth_jmp_rax_with_targets(2);
    let (g, _, _) = common::analyze_with_known_targets(&bytes, base, ba, &targets);
    // 2-target Switch: zero Ifs, zero equality cmps, exactly one Switch
    // node with 2 Control outputs.
    assert_eq!(
        common::count_ifs(&g),
        0,
        "2-target Switch produces zero Ifs"
    );
    assert_eq!(count_eq_cmps(&g), 0, "2-target Switch produces zero cmps");
    assert_eq!(
        count_switches(&g),
        1,
        "2-target dispatch lifts to exactly one Switch node",
    );
    let switch_id = find_unique_switch(&g);
    assert_eq!(
        g.node_outputs(switch_id).len(),
        2,
        "Switch has one Control output per target",
    );
    assert_eq!(
        g.side_tables().switch_targets(switch_id),
        targets.as_slice(),
        "switch_targets side table must list both target addresses in target order",
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
