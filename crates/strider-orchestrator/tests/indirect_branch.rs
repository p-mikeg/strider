//! `fixtures/cases/indirect_branch.c::indirect_branch_resolved` lowers the
//! indirect goto to a load from a local stack array of label addresses on
//! every supported toolchain/optimisation level we target (gcc/clang
//! collapse direct constant computed-gotos into straight-line `mov; ret`,
//! so the surviving lowering is always: write label addresses to the local
//! stack array, load the per-iteration target from the array, branch
//! through the loaded value).
//!
//! Resolving this requires cross-region stack-load forwarding
//! (`StackOffsetDetect` + `LoadForward` joined across the function's region
//! graph), routed through the IR-level resolver's unified table-dispatch
//! arm (`indirect_branch_resolve::table::classify_table_dispatch`, SP-rooted
//! base). Cfg-time the builder defers every `BranchIndirect` via
//! `UnresolvedIndirectBranch`; the IR-level resolver has cross-region
//! visibility plus `LoadForward` results and resolves the dispatch to
//! `ResolvedTargets::Multiple`.

mod common;
use common::*;
use strider_ir::{IRViewer, IRWalker};

/// `indirect_branch_resolved` over the default ROM: the runtime-immutable
/// image, where a table in a writable section is unmapped.
fn assert_indirect_goto_resolves_to_both_labels(arch: Arch) {
    resolves_to_both_labels(
        arch,
        fixture_strider(arch, "indirect_branch", "indirect_branch_resolved"),
    );
}

/// The same over a ROM that also serves the relocated GOT. A PIC dispatch
/// loads its table base through the GOT, so nothing in it folds while those
/// slots are unmapped.
fn assert_indirect_goto_resolves_with_the_got_served(arch: Arch) {
    resolves_to_both_labels(
        arch,
        fixture_strider_serving_got(arch, "indirect_branch", "indirect_branch_resolved"),
    );
}

/// Drives `Strider::analyze` to its fixed point and asserts the site resolved
/// to BOTH of the fixture's computed-goto labels: no region left carrying
/// `RegionTerminator::UnresolvedIndirectBranch`, no `IndirectBranch`
/// placeholder left in the IR, exactly one `Switch` and exactly two distinct
/// arms, each of which starts a region of the final cfg.
///
/// The arm addresses are not spelled out here (they differ per arch), but a
/// one-arm answer, an over-approximated table, and an arm landing off a
/// region start all fail.
fn resolves_to_both_labels(
    arch: Arch,
    (mut strider, cc, entry): (
        strider_orchestrator::Strider<strider_reader::ElfFileMemReader>,
        strider_target::BuiltCallingConvention,
        u64,
    ),
) {
    let result = strider
        .analyze(
            entry,
            &cc,
            &strider_orchestrator::LiftOptions::default(),
            &strider_orchestrator::opt::OptOptions::default(),
            None,
        )
        .unwrap_or_else(|e| panic!("analyze on {}: {e:?}", arch.name()));

    assert!(
        result.unresolved_indirect_branches.is_empty(),
        "{}: unresolved {:#x?}",
        arch.name(),
        result
            .unresolved_indirect_branches
            .iter()
            .map(|a| a.machine_addr.addr)
            .collect::<Vec<_>>(),
    );
    let live_placeholders = result
        .function
        .walk()
        .filter(|&n| {
            matches!(
                result.function.node_kind(n),
                strider_ir::node::NodeKind::IndirectBranch
            )
        })
        .count();
    assert_eq!(
        live_placeholders,
        0,
        "{}: IndirectBranch placeholder still live in the IR",
        arch.name()
    );

    let mut arms: Vec<Vec<u64>> = Vec::new();
    let mut region_starts: Vec<u64> = Vec::new();
    for region in result.cfg.regions() {
        region_starts.push(region.start_addr.machine_addr.addr);
        match &region.terminator {
            strider_cfg::RegionTerminator::UnresolvedIndirectBranch { addr, .. } => panic!(
                "{}: region still carries UnresolvedIndirectBranch at {:#x}",
                arch.name(),
                addr.machine_addr.addr
            ),
            strider_cfg::RegionTerminator::Switch { targets, .. } => {
                arms.push(targets.iter().map(|t| t.addr).collect());
            }
            _ => {}
        }
    }
    assert_eq!(
        arms.len(),
        1,
        "{}: expected the one computed goto to be the only Switch, got {arms:#x?}",
        arch.name()
    );
    let mut resolved = arms.remove(0);
    resolved.sort_unstable();
    resolved.dedup();
    assert_eq!(
        resolved.len(),
        2,
        "{}: the fixture has two labels, resolved {resolved:#x?}",
        arch.name()
    );
    for target in &resolved {
        assert!(
            region_starts.contains(target),
            "{}: arm {target:#x} starts no region; arms {resolved:#x?}",
            arch.name()
        );
    }
}

#[test]
fn indirect_branch_resolved_x86() {
    assert_indirect_goto_resolves_to_both_labels(Arch::X86);
}
#[test]
fn indirect_branch_resolved_x64() {
    assert_indirect_goto_resolves_to_both_labels(Arch::X64);
}
#[test]
fn indirect_branch_resolved_aarch64() {
    assert_indirect_goto_resolves_to_both_labels(Arch::Aarch64);
}
#[test]
#[ignore = "aarch64-be: stack-array dispatch unresolved; the table base comes out of a `bfi` insert against an alignment-masked SP, a shape the SP-decomposition does not spell out, so the classifier never sees a stack base to probe"]
fn indirect_branch_resolved_aarch64be() {
    assert_indirect_goto_resolves_to_both_labels(Arch::Aarch64Be);
}
#[test]
fn indirect_branch_resolved_arm() {
    assert_indirect_goto_resolves_to_both_labels(Arch::Arm);
}
#[test]
fn indirect_branch_resolved_arm_be() {
    assert_indirect_goto_resolves_to_both_labels(Arch::ArmBe);
}
#[test]
fn indirect_branch_resolved_arm_thumb() {
    assert_indirect_goto_resolves_to_both_labels(Arch::ArmThumb);
}
#[test]
fn indirect_branch_resolved_mips32le() {
    assert_indirect_goto_resolves_to_both_labels(Arch::Mips32le);
}
#[test]
fn indirect_branch_resolved_mips32be() {
    assert_indirect_goto_resolves_to_both_labels(Arch::Mips32be);
}
#[test]
fn indirect_branch_resolved_mips64le() {
    assert_indirect_goto_resolves_with_the_got_served(Arch::Mips64le);
}
#[test]
fn indirect_branch_resolved_mips64be() {
    assert_indirect_goto_resolves_with_the_got_served(Arch::Mips64be);
}
#[test]
fn indirect_branch_resolved_ppc32be() {
    assert_indirect_goto_resolves_to_both_labels(Arch::Ppc32be);
}
#[test]
fn indirect_branch_resolved_ppc32le() {
    // The label table is an eight-byte `.data.rel.ro`, so the dispatch needs
    // the GOT-serving ROM; with the immutable-only one the base is unmapped.
    assert_indirect_goto_resolves_with_the_got_served(Arch::Ppc32le);
}
#[test]
fn indirect_branch_resolved_ppc64be() {
    // Same shape as ppc32le: the label table is a `.data.rel.ro` the
    // immutable-only ROM leaves unmapped, so the base never folds without it.
    assert_indirect_goto_resolves_with_the_got_served(Arch::Ppc64be);
}
#[test]
fn indirect_branch_resolved_ppc64le() {
    assert_indirect_goto_resolves_with_the_got_served(Arch::Ppc64le);
}
