//! strider lifts a `RegionTerminator::UnresolvedIndirectBranch`
//! region by emitting a placeholder `IndirectBranch(target_value)`
//! that anchors the dispatch varnode in the IR for the indirect-
//! branch resolver.
//!
//! The test drives a synthetic x86-64 `jmp rax` CFG (RAX is a
//! function-entry value; the cfg builder does no cfg-time resolution,
//! so the site is deferred via `UnresolvedIndirectBranch`).  Pre-fix,
//! `build_ir` either errored or emitted an
//! ABI Return that discarded the dispatch value.  Post-fix, it
//! succeeds and produces an IR with exactly one IndirectBranch node
//! whose single value-input is `target_vn`'s value at the
//! BranchIndirect site.
//!
//! These tests intentionally do NOT use the per-arch fixture suite —
//! that infrastructure runs the full optimizer pipeline against a real
//! ELF.  This is a per-region lifting concern; we use a direct
//! `Builder + Lifter::new + build_ir` call sequence so the test
//! exercises *only* the strider IR-lift step.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use rsleigh::mem_readers::BufMemReader;
use strider_cfg::MachineInsnAddr;
use strider_ir::{IRViewer, IRWalker};
use strider_orchestrator::Lifter;

mod common;

/// Build a synthetic x86-64 CFG (with the driver that owns its Sleigh)
/// containing a single region whose terminator is
/// `UnresolvedIndirectBranch{target_vn=RAX, addr=...}`.
///
/// Bytes: `0xff 0xe0` — `jmp rax`.  RAX is the function-entry value of
/// the dispatch register; cfg-time resolver cannot classify (no LR is set, no
/// constant write to RAX), so the cfg builder defers via the the cfg-time placeholder lift
/// fall-through and we end up with the new terminator.
///
/// Returns `(driver, cfg, cc)` — the driver OWNS the Sleigh used to
/// build `cfg`, so it must also be the one that lifts it.
fn make_unresolved_indirect_branch_cfg() -> (
    Lifter<BufMemReader<Vec<u8>>>,
    strider_cfg::Cfg,
    strider_target::BuiltCallingConvention,
) {
    let base = 0x1000u64;
    let bytes: Vec<u8> = vec![0xff, 0xe0];
    let reader = BufMemReader::new(bytes, base);
    // No link-register on x86-64 (the cdecl-family conventions push the
    // return address onto the stack), so cfg-time resolver's LinkRegister arm
    // can't classify either.
    let (mut driver, cc) = common::strider_x86_64(reader);
    let cfg = driver
        .build_cfg(
            MachineInsnAddr::from(base),
            &strider_cfg::CfgOptions::default(),
        )
        .expect("cfg build must succeed under the cfg-time placeholder lift deferral");
    (driver, cfg, cc)
}

/// Placeholder contract: a region terminated with
/// `UnresolvedIndirectBranch` lifts to an IR that is well-formed
/// (no error, one IndirectBranch node).  Pre-restructure, the strider
/// lifter dispatched the `BranchIndirect` opcode to `handle_return`,
/// which produced an ABI Return whose inputs were the convention's
/// `ret_val_regs` — NOT the dispatch varnode.  Post-fix, strider
/// inspects the region's terminator and emits an
/// `IndirectBranch(target_value)` placeholder that anchors `target_vn`
/// in the IR.
///
/// Side-effect anchor expectation: the IR's unique IndirectBranch
/// must have a value-input slot wired (the placeholder anchors
/// target_value at slot 2 — slots 0/1 are control/memory).
#[test]
fn unresolvable_branch_indirect_lifts_as_return_placeholder() {
    let (strider, cfg, cc) = make_unresolved_indirect_branch_cfg();
    let function = strider
        .build_ir(&cfg, &cc)
        .expect("strider must lift unresolved branches as IndirectBranch placeholder")
        .function;

    // Exactly one IndirectBranch node — strider emitted the
    // placeholder, did not double-emit, and did not lift the
    // BranchIndirect via the pre-fix ABI handle_return path.
    let placeholder_count = function
        .walk()
        .filter(|nid| {
            matches!(
                function.node_kind(*nid),
                strider_ir::node::NodeKind::IndirectBranch
            )
        })
        .count();
    assert_eq!(
        placeholder_count, 1,
        "expected exactly one IndirectBranch placeholder, got {placeholder_count}"
    );

    // The placeholder must have a value-input slot wired — its layout
    // is [control, memory, target_value].  That's exactly 3 inputs.
    let placeholder = function
        .walk()
        .find(|nid| {
            matches!(
                function.node_kind(*nid),
                strider_ir::node::NodeKind::IndirectBranch
            )
        })
        .expect("must have an IndirectBranch node");
    let inputs = function.node_inputs(placeholder);
    assert_eq!(
        inputs.len(),
        3,
        "placeholder must have layout [control, memory, target_value]; got {} inputs",
        inputs.len()
    );
}

/// `known_targets[addr] = Single(oob)` on a synthetic `jmp rax` fixture:
/// the CFG builder seats a `TailCall { target: oob }` terminator, and the
/// lift driver materialises it as a `Call(IntConst(oob)) + Return` IR pair.
///
/// This exercises the full path from the resolution-map feedback through the
/// CFG-terminator seating into the IR materialisation — the same path the
/// orchestrator's fixed-point loop uses once it resolves an indirect branch
/// to an out-of-function target.
#[test]
fn known_single_oob_target_lifts_as_call_plus_return() {
    use rustc_hash::FxHashMap;
    use strider_cfg::{PcodeInsnAddr, ResolvedTargets};
    use strider_ir::IRWalker;
    use strider_ir::node::NodeKind;

    let base = 0x1000u64;
    let oob_target = 0x9000u64;

    // `jmp rax` (0xff 0xe0) followed by int3 padding so the
    // BufMemReader doesn't fault on speculative look-ahead.
    let mut bytes = vec![0xff, 0xe0u8];
    bytes.extend(std::iter::repeat_n(0xccu8, 16));

    let reader = rsleigh::mem_readers::BufMemReader::new(bytes, base);
    // The driver OWNS the Sleigh and rebuilds the CFG across both passes.
    let (mut strider, cc) = common::strider_x86_64(reader);

    // First pass: build without known_targets to locate the
    // UnresolvedIndirectBranch pcode address.
    let unresolved_addr = {
        let cfg_opts = strider_cfg::CfgOptions {
            fn_max_size: Some(0x100),
            ..Default::default()
        };
        let cfg_v1 = strider
            .build_cfg(MachineInsnAddr::from(base), &cfg_opts)
            .expect("initial cfg build");
        cfg_v1
            .regions()
            .find_map(|r| {
                if let strider_cfg::RegionTerminator::UnresolvedIndirectBranch { addr, .. } =
                    r.terminator
                {
                    Some(addr)
                } else {
                    None
                }
            })
            .expect("initial cfg must have an UnresolvedIndirectBranch region")
    };

    // Second pass: seed known_targets with Single(oob) so the builder
    // emits TailCall { target: oob_target }.
    let mut known: FxHashMap<PcodeInsnAddr, ResolvedTargets> = FxHashMap::default();
    known.insert(unresolved_addr, ResolvedTargets::Single(oob_target));

    let cfg_opts2 = strider_cfg::CfgOptions {
        fn_max_size: Some(0x100),
        known_targets: known,
        ..Default::default()
    };
    let cfg = strider
        .build_cfg(MachineInsnAddr::from(base), &cfg_opts2)
        .expect("cfg with Single(oob) known_target");

    // Confirm the CFG-level terminator is TailCall before lifting.
    let has_tail_call = cfg
        .regions()
        .any(|r| matches!(r.terminator, strider_cfg::RegionTerminator::TailCall { target } if target == oob_target));
    assert!(
        has_tail_call,
        "CFG must have TailCall {{ target: {oob_target:#x} }} before lifting"
    );

    // Lift to IR and verify Call + Return are both present.
    let function = strider
        .build_ir(&cfg, &cc)
        .expect("build_ir with TailCall terminator from known_targets must succeed")
        .function;

    let call_count = function.count_kind(|k| matches!(k, NodeKind::Call));
    assert_eq!(
        call_count, 1,
        "TailCall terminator must lift to exactly one Call node"
    );

    let return_count = function.count_kind(|k| matches!(k, NodeKind::Return));
    assert_eq!(
        return_count, 1,
        "TailCall terminator must lift to exactly one Return node"
    );

    // The Call's target must be IntConst(oob_target).
    let has_oob_const = function.walk().any(|nid| {
        matches!(function.node_kind(nid), NodeKind::IntConst(_))
            && function
                .first_value_output_of(nid)
                .is_some_and(|v| function.int_const_u128(v) == Some(u128::from(oob_target)))
    });
    assert!(
        has_oob_const,
        "lifted IR must contain IntConst({oob_target:#x}) as the Call target"
    );
}

/// `known_targets[addr] = Single(intra)` on a `jmp rax` whose resolved target
/// is an INTRA-function address: the CFG builder must seat an `Unconditional`
/// terminator with an edge to the target region, and the lift must NOT emit a
/// spurious `Return` for the jump region.  The only `Return` in the lifted IR
/// is the one from the real `ret` at the resolved target.
///
/// Regression: the `Unconditional` branch of `finish_branch_or_tail_call` used
/// to leave the trailing `BranchIndirect` p-code insn in the region, which the
/// IR per-region loop then routed through `handle_return` (Return and
/// BranchIndirect share a dispatch arm), producing a region that both returned
/// AND had a forward control edge (`return; goto succ`) — a silent mis-lift the
/// validator does not catch.
#[test]
fn known_single_intra_target_lifts_as_unconditional_no_spurious_return() {
    use rustc_hash::FxHashMap;
    use strider_cfg::{PcodeInsnAddr, ResolvedTargets};
    use strider_ir::IRWalker;
    use strider_ir::node::NodeKind;

    let base = 0x1000u64;
    let intra_target = 0x1002u64; // within [base, base + fn_max_size)

    // 0x1000: `jmp rax` (0xff 0xe0).  0x1002: `ret` (0xc3) — the resolved
    // intra-function target.  Trailing int3 padding for speculative look-ahead.
    let mut bytes = vec![0xff, 0xe0u8, 0xc3u8];
    bytes.extend(std::iter::repeat_n(0xccu8, 16));

    let reader = rsleigh::mem_readers::BufMemReader::new(bytes, base);
    let (mut strider, cc) = common::strider_x86_64(reader);

    // First pass: locate the UnresolvedIndirectBranch pcode address.
    let unresolved_addr = {
        let cfg_opts = strider_cfg::CfgOptions {
            fn_max_size: Some(0x100),
            ..Default::default()
        };
        let cfg_v1 = strider
            .build_cfg(MachineInsnAddr::from(base), &cfg_opts)
            .expect("initial cfg build");
        cfg_v1
            .regions()
            .find_map(|r| {
                if let strider_cfg::RegionTerminator::UnresolvedIndirectBranch { addr, .. } =
                    r.terminator
                {
                    Some(addr)
                } else {
                    None
                }
            })
            .expect("initial cfg must have an UnresolvedIndirectBranch region")
    };

    // Second pass: seed known_targets with Single(intra).
    let mut known: FxHashMap<PcodeInsnAddr, ResolvedTargets> = FxHashMap::default();
    known.insert(unresolved_addr, ResolvedTargets::Single(intra_target));

    let cfg_opts2 = strider_cfg::CfgOptions {
        fn_max_size: Some(0x100),
        known_targets: known,
        ..Default::default()
    };
    let cfg = strider
        .build_cfg(MachineInsnAddr::from(base), &cfg_opts2)
        .expect("cfg with Single(intra) known_target");

    // CFG: the jump region is Unconditional (NOT a tail call), and the target
    // region at `intra_target` is explored.
    let has_unconditional = cfg
        .regions()
        .any(|r| matches!(r.terminator, strider_cfg::RegionTerminator::Unconditional));
    assert!(
        has_unconditional,
        "intra-function Single must seat an Unconditional terminator"
    );
    let has_tail_call = cfg
        .regions()
        .any(|r| matches!(r.terminator, strider_cfg::RegionTerminator::TailCall { .. }));
    assert!(
        !has_tail_call,
        "intra-function Single must NOT be a tail call"
    );

    let function = strider
        .build_ir(&cfg, &cc)
        .expect("build_ir with Unconditional terminator from intra Single")
        .function;

    // The ONLY Return is the real `ret` at intra_target; the jump region must
    // not emit a second, spurious Return.
    let return_count = function.count_kind(|k| matches!(k, NodeKind::Return));
    assert_eq!(
        return_count, 1,
        "intra-function resolved jump must lift to exactly one Return (the real `ret`), \
         not a spurious extra Return from the leftover BranchIndirect"
    );
    // And no Call — an intra-function jump is neither a call nor a tail call.
    let call_count = function.count_kind(|k| matches!(k, NodeKind::Call));
    assert_eq!(call_count, 0, "intra-function jump must not lift to a Call");
}

/// Anchor-tracking contract: the strider exposes a side-table
/// mapping each placeholder's pcode address to the `ValueId`
/// that anchors `target_vn`.  the IR-level orchestrator resolver
/// walks this table.
#[test]
fn unresolved_branches_table_tracks_each_placeholder() {
    let (strider, cfg, cc) = make_unresolved_indirect_branch_cfg();
    let outcome = strider.build_ir(&cfg, &cc).expect("build_ir");
    // Single deferred branch in this synthetic fixture.
    assert_eq!(
        outcome.unresolved_branches.len(),
        1,
        "expected exactly one tracked placeholder, got {}",
        outcome.unresolved_branches.len(),
    );
    // The tracked address must point at the original BranchIndirect.
    let (addr, _placeholder) = outcome.unresolved_branches[0];
    assert_eq!(addr.machine_addr.addr, 0x1000);
}
