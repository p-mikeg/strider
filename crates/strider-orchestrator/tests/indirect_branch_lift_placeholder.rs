//! strider lifts a `RegionTerminator::UnresolvedIndirectBranch`
//! region by emitting a placeholder `IndirectBranch(target_value)`
//! that anchors the dispatch varnode in the IR for the indirect-
//! branch resolver.
//!
//! The test drives a synthetic x86-64 `jmp rax` CFG (RAX is a
//! function-entry value; the cfg builder does no cfg-time resolution,
//! so the site is deferred via `UnresolvedIndirectBranch`).  Pre-fix,
//! `analyze_cfg` either errored or emitted an
//! ABI Return that discarded the dispatch value.  Post-fix, it
//! succeeds and produces an IR with exactly one IndirectBranch node
//! whose single value-input is `target_vn`'s value at the
//! BranchIndirect site.
//!
//! These tests intentionally do NOT use the per-arch fixture suite —
//! that infrastructure runs the full optimizer pipeline against a real
//! ELF.  This is a per-region lifting concern; we use a direct
//! `Builder + LiftDriver::new + analyze_cfg` call sequence so the test
//! exercises *only* the strider IR-lift step.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use rsleigh::Sleigh;
use strider_ir::{IRViewer, IRWalker};
use rsleigh::mem_readers::BufMemReader;
use strider_cfg::Builder;
use strider_lift::LiftOptions;
use strider_target::SleighArch;

mod common;

/// Build a synthetic x86-64 CFG containing a single region whose
/// terminator is `UnresolvedIndirectBranch{target_vn=RAX, addr=...}`.
///
/// Bytes: `0xff 0xe0` — `jmp rax`.  RAX is the function-entry value of
/// the dispatch register; cfg-time resolver cannot classify (no LR is set, no
/// constant write to RAX), so the cfg builder defers via the the cfg-time placeholder lift
/// fall-through and we end up with the new terminator.
fn make_unresolved_indirect_branch_cfg() -> (
    strider_cfg::Cfg,
    Sleigh<BufMemReader<Vec<u8>>>,
    SleighArch,
) {
    let base = 0x1000u64;
    let bytes: Vec<u8> = vec![0xff, 0xe0];
    let arch = SleighArch::x86_64();
    let reader = BufMemReader::new(bytes, base);
    let mut sleigh =
        Sleigh::new(arch.sla_spec(), arch.pspec(), reader).expect("create x86-64 sleigh");
    // No link-register on x86-64 (the cdecl-family conventions push the
    // return address onto the stack), so cfg-time resolver's LinkRegister arm
    // can't classify either.
    let opts = LiftOptions::default();
    let cfg = Builder::for_arch(&arch, &mut sleigh, base, &opts.cfg)
        .build()
        .expect("cfg build must succeed under the cfg-time placeholder lift deferral");
    (cfg, sleigh, arch)
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
    let (cfg, sleigh, arch) = make_unresolved_indirect_branch_cfg();
    let _ = arch; // arch is the SleighArch the cfg was built with; unused here
    let strider = common::strider_x86_64();
    let function = strider
        .analyze_cfg(&cfg, &sleigh)
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
    use strider_ir::IRWalker;
    use strider_ir::node::NodeKind;
    use strider_cfg::{Builder, PcodeInsnAddr, ResolvedTargets};

    let base = 0x1000u64;
    let oob_target = 0x9000u64;

    // `jmp rax` (0xff 0xe0) followed by int3 padding so the
    // BufMemReader doesn't fault on speculative look-ahead.
    let mut bytes = vec![0xff, 0xe0u8];
    bytes.extend(std::iter::repeat_n(0xccu8, 16));

    let arch = strider_target::SleighArch::x86_64();
    let reader = rsleigh::mem_readers::BufMemReader::new(bytes, base);
    let mut sleigh = rsleigh::Sleigh::new(arch.sla_spec(), arch.pspec(), reader)
        .expect("create x86_64 sleigh");

    // First pass: build without known_targets to locate the
    // UnresolvedIndirectBranch pcode address.
    let unresolved_addr = {
        let opts = LiftOptions {
            cfg: strider_cfg::CfgOptions {
                fn_max_size: Some(0x100),
                ..Default::default()
            },
            ..LiftOptions::default()
        };
        let cfg_v1 = Builder::for_arch(&arch, &mut sleigh, base, &opts.cfg)
            .build()
            .expect("initial cfg build");
        cfg_v1
            .regions()
            .find_map(|r| {
                if let strider_cfg::RegionTerminator::UnresolvedIndirectBranch { addr, .. } = r.terminator {
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

    let mut bytes2: Vec<u8> = vec![0xff, 0xe0u8];
    bytes2.extend(std::iter::repeat_n(0xccu8, 16));
    let reader2 = rsleigh::mem_readers::BufMemReader::new(bytes2, base);
    let mut sleigh2 = rsleigh::Sleigh::new(arch.sla_spec(), arch.pspec(), reader2)
        .expect("create x86_64 sleigh (pass 2)");
    let opts2 = LiftOptions {
        cfg: strider_cfg::CfgOptions {
            fn_max_size: Some(0x100),
            known_targets: known,
            ..Default::default()
        },
        ..LiftOptions::default()
    };
    let cfg = Builder::for_arch(&arch, &mut sleigh2, base, &opts2.cfg)
        .build()
        .expect("cfg with Single(oob) known_target");

    // Confirm the CFG-level terminator is TailCall before lifting.
    let has_tail_call = cfg
        .regions()
        .any(|r| matches!(r.terminator, strider_cfg::RegionTerminator::TailCall { target } if target == oob_target));
    assert!(has_tail_call, "CFG must have TailCall {{ target: {oob_target:#x} }} before lifting");

    // Lift to IR and verify Call + Return are both present.
    let strider = common::strider_x86_64();
    let function = strider
        .analyze_cfg(&cfg, &sleigh2)
        .expect("analyze_cfg with TailCall terminator from known_targets must succeed")
        .function;

    let call_count = function.count_kind(|k| matches!(k, NodeKind::Call));
    assert_eq!(call_count, 1, "TailCall terminator must lift to exactly one Call node");

    let return_count = function.count_kind(|k| matches!(k, NodeKind::Return));
    assert_eq!(return_count, 1, "TailCall terminator must lift to exactly one Return node");

    // The Call's target must be IntConst(oob_target).
    use strider_ir::node::IntPayload;
    let has_oob_const = function.has_kind(|k| {
        matches!(k, NodeKind::IntConst(IntPayload::Small(c)) if *c == oob_target)
    });
    assert!(
        has_oob_const,
        "lifted IR must contain IntConst({oob_target:#x}) as the Call target"
    );
}

/// Anchor-tracking contract: the strider exposes a side-table
/// mapping each placeholder's pcode address to the `ValueId`
/// that anchors `target_vn`.  the IR-level orchestrator resolver
/// walks this table.
#[test]
fn unresolved_branches_table_tracks_each_placeholder() {
    let (cfg, sleigh, arch) = make_unresolved_indirect_branch_cfg();
    let _ = arch; // arch is the SleighArch the cfg was built with; unused here
    let strider = common::strider_x86_64();
    let outcome = strider.analyze_cfg(&cfg, &sleigh).expect("analyze_cfg");
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
