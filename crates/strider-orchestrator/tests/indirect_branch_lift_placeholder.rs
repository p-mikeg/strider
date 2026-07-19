//! strider lifts a `RegionTerminator::UnresolvedIndirectBranch` region by
//! emitting a placeholder `IndirectBranch(target_value)` that targets the
//! dispatch varnode, for the indirect-branch resolver to consume later.
//!
//! Drives a synthetic x86-64 `jmp rax` CFG (RAX is a function-entry
//! value; the cfg builder does no cfg-time resolution, so the site is
//! deferred via `UnresolvedIndirectBranch`). Pre-fix, `build_ir` either
//! errored or emitted an ABI Return that discarded the dispatch value.
//! Post-fix it produces an IR with exactly one IndirectBranch node whose
//! single value-input is `target_vn`'s value at the BranchIndirect site.
//!
//! Bypasses the per-arch fixture suite (which runs the full optimizer
//! pipeline against a real ELF) in favor of a direct `Builder +
//! Lifter::new + build_ir` call sequence, since this is a per-region
//! lifting concern only.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use rsleigh::mem_readers::BufMemReader;
use strider_cfg::MachineInsnAddr;
use strider_ir::{IRViewer, IRWalker};
use strider_orchestrator::Lifter;

mod common;

/// `0xff 0xe0` (`jmp rax`) with RAX the function-entry value: the
/// cfg-time resolver can't classify it (no LR set, no constant write to
/// RAX), so the builder defers with `UnresolvedIndirectBranch`.
///
/// Returns `(driver, cfg, cc)`; the driver OWNS the Sleigh used to build
/// `cfg`, so it must also be the one that lifts it.
fn make_unresolved_indirect_branch_cfg() -> (
    Lifter<BufMemReader<Vec<u8>>>,
    strider_cfg::Cfg,
    strider_target::BuiltCallingConvention,
) {
    let base = 0x1000u64;
    let bytes: Vec<u8> = vec![0xff, 0xe0];
    let reader = BufMemReader::new(bytes, base);
    // No link register on x86-64 (cdecl-family conventions push the return
    // address onto the stack), so the LinkRegister classifier arm can't
    // classify this either.
    let (mut driver, cc) = common::strider_x86_64(reader);
    let cfg = driver
        .build_cfg(
            MachineInsnAddr::from(base),
            &strider_cfg::CfgOptions::default(),
            &Default::default(),
        )
        .expect("cfg build must succeed under the cfg-time placeholder lift deferral");
    (driver, cfg, cc)
}

/// Regression: the lifter used to dispatch `BranchIndirect` to
/// `handle_return`, producing an ABI Return whose inputs were the
/// convention's `ret_val_regs`, not the dispatch varnode. Now it inspects
/// the region's terminator and emits an `IndirectBranch(target_value)`
/// placeholder wired to `target_vn` at slot 2 (slots 0/1 are
/// control/memory).
#[test]
fn unresolvable_branch_indirect_lifts_as_return_placeholder() {
    let (strider, cfg, cc) = make_unresolved_indirect_branch_cfg();
    let function = strider
        .build_ir(&cfg, cc)
        .expect("strider must lift unresolved branches as IndirectBranch placeholder")
        .function;

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

    // Layout is [control, memory, target_value]: exactly 3 inputs.
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
/// Exercises the same path the orchestrator's fixed-point loop uses once it
/// resolves an indirect branch to an out-of-function target: resolution-map
/// feedback, CFG-terminator seating, IR materialisation.
#[test]
fn known_single_oob_target_lifts_as_call_plus_return() {
    use rustc_hash::FxHashMap;
    use strider_cfg::{PcodeInsnAddr, ResolvedTargets};
    use strider_ir::node::NodeKind;
    use strider_ir_test_utils::IrWalkerEx;

    let base = 0x1000u64;
    let oob_target = 0x9000u64;

    // `jmp rax` (0xff 0xe0) followed by int3 padding so the
    // BufMemReader doesn't fault on speculative look-ahead.
    let mut bytes = vec![0xff, 0xe0u8];
    bytes.extend(std::iter::repeat_n(0xccu8, 16));

    let reader = rsleigh::mem_readers::BufMemReader::new(bytes, base);
    // The driver OWNS the Sleigh and rebuilds the CFG across both passes.
    let (mut strider, cc) = common::strider_x86_64(reader);

    let unresolved_addr = {
        let cfg_opts = strider_cfg::CfgOptions {
            fn_max_size: Some(0x100),
            ..Default::default()
        };
        let cfg_v1 = strider
            .build_cfg(MachineInsnAddr::from(base), &cfg_opts, &Default::default())
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

    // Seed known_targets with Single(oob) so the builder emits
    // TailCall { target: oob_target }.
    let mut known: FxHashMap<PcodeInsnAddr, ResolvedTargets> = FxHashMap::default();
    known.insert(unresolved_addr, ResolvedTargets::Single(oob_target));

    let cfg_opts2 = strider_cfg::CfgOptions {
        fn_max_size: Some(0x100),
        known_targets: known,
        ..Default::default()
    };
    let cfg = strider
        .build_cfg(MachineInsnAddr::from(base), &cfg_opts2, &Default::default())
        .expect("cfg with Single(oob) known_target");

    let has_tail_call = cfg
        .regions()
        .any(|r| matches!(r.terminator, strider_cfg::RegionTerminator::TailCall { target } if target == oob_target));
    assert!(
        has_tail_call,
        "CFG must have TailCall {{ target: {oob_target:#x} }} before lifting"
    );

    let function = strider
        .build_ir(&cfg, cc)
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

/// `known_targets[addr] = Single(intra)` on a `jmp rax` whose resolved
/// target is an INTRA-function address: the CFG builder must seat an
/// `Unconditional` terminator with an edge to the target region, and the
/// lift must NOT emit a spurious `Return` for the jump region; the only
/// `Return` in the lifted IR is the real `ret` at the resolved target.
///
/// Regression: the `Unconditional` branch of `finish_branch_or_tail_call`
/// used to leave the trailing `BranchIndirect` p-code insn in the region,
/// which the IR per-region loop then routed through `handle_return`
/// (Return and BranchIndirect share a dispatch arm), producing a region
/// that both returned AND had a forward control edge (`return; goto
/// succ`), a silent mis-lift the validator does not catch.
#[test]
fn known_single_intra_target_lifts_as_unconditional_no_spurious_return() {
    use rustc_hash::FxHashMap;
    use strider_cfg::{PcodeInsnAddr, ResolvedTargets};
    use strider_ir::node::NodeKind;
    use strider_ir_test_utils::IrWalkerEx;

    let base = 0x1000u64;
    let intra_target = 0x1002u64; // within [base, base + fn_max_size)

    // 0x1000: `jmp rax` (0xff 0xe0). 0x1002: `ret` (0xc3), the resolved
    // intra-function target. Trailing int3 padding for speculative look-ahead.
    let mut bytes = vec![0xff, 0xe0u8, 0xc3u8];
    bytes.extend(std::iter::repeat_n(0xccu8, 16));

    let reader = rsleigh::mem_readers::BufMemReader::new(bytes, base);
    let (mut strider, cc) = common::strider_x86_64(reader);

    let unresolved_addr = {
        let cfg_opts = strider_cfg::CfgOptions {
            fn_max_size: Some(0x100),
            ..Default::default()
        };
        let cfg_v1 = strider
            .build_cfg(MachineInsnAddr::from(base), &cfg_opts, &Default::default())
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

    let mut known: FxHashMap<PcodeInsnAddr, ResolvedTargets> = FxHashMap::default();
    known.insert(unresolved_addr, ResolvedTargets::Single(intra_target));

    let cfg_opts2 = strider_cfg::CfgOptions {
        fn_max_size: Some(0x100),
        known_targets: known,
        ..Default::default()
    };
    let cfg = strider
        .build_cfg(MachineInsnAddr::from(base), &cfg_opts2, &Default::default())
        .expect("cfg with Single(intra) known_target");

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
        .build_ir(&cfg, cc)
        .expect("build_ir with Unconditional terminator from intra Single")
        .function;

    let return_count = function.count_kind(|k| matches!(k, NodeKind::Return));
    assert_eq!(
        return_count, 1,
        "intra-function resolved jump must lift to exactly one Return (the real `ret`), \
         not a spurious extra Return from the leftover BranchIndirect"
    );
    let call_count = function.count_kind(|k| matches!(k, NodeKind::Call));
    assert_eq!(call_count, 0, "intra-function jump must not lift to a Call");
}

/// `unresolved_branches` maps each placeholder's pcode address to the
/// `ValueId` targeting `target_vn`; the IR-level orchestrator resolver
/// walks this table.
#[test]
fn unresolved_branches_table_tracks_each_placeholder() {
    let (strider, cfg, cc) = make_unresolved_indirect_branch_cfg();
    let outcome = strider.build_ir(&cfg, cc).expect("build_ir");
    assert_eq!(
        outcome.unresolved_branches.len(),
        1,
        "expected exactly one tracked placeholder, got {}",
        outcome.unresolved_branches.len(),
    );
    let (addr, _placeholder) = outcome.unresolved_branches[0];
    assert_eq!(addr.machine_addr.addr, 0x1000);
}
