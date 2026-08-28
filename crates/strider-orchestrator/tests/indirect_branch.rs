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
//! arm (`strider_orchestrator::opt::classify_table_dispatch`, SP-rooted
//! base). Cfg-time the builder defers every `BranchIndirect` via
//! `UnresolvedIndirectBranch`; the IR-level resolver has cross-region
//! visibility plus `LoadForward` results and resolves the dispatch to
//! `ResolvedTargets::Multiple`.

mod common;
use common::*;
use object::{Object, ObjectSymbol};
use strider_ir::{IRViewer, IRWalker};

/// Drives `Strider::analyze` to its fixed point on
/// `indirect_branch_resolved` and asserts the site resolved to BOTH of the
/// fixture's computed-goto labels: no region left carrying
/// `RegionTerminator::UnresolvedIndirectBranch`, no `IndirectBranch`
/// placeholder left in the IR, exactly one `Switch` and exactly two distinct
/// arms, each of which starts a region of the final cfg.
///
/// The arm addresses are not spelled out here (they differ per arch), but a
/// one-arm answer, an over-approximated table, and an arm landing off a
/// region start all fail.
fn assert_indirect_goto_resolves_to_both_labels(arch: Arch) {
    let path = binary_path(arch, "indirect_branch");
    let owned = strider_reader::load_elf(&path)
        .unwrap_or_else(|e| panic!("load_elf({path:?}) failed: {e:?}"));
    let obj = owned.file();
    let sleigh_arch = arch.sleigh();
    // The Thumb interworking bit IS the entry's ISA mode; `build_cfg` masks it
    // off for decoding itself.
    let entry = obj
        .symbol_by_name("indirect_branch_resolved")
        .unwrap_or_else(|| panic!("symbol not found in {path:?}"))
        .address();
    let mem = strider_reader::ElfFileMemReader::from_object(&obj).expect("mem reader");
    let sleigh = rsleigh::Sleigh::new(sleigh_arch.sla_spec(), sleigh_arch.pspec(), mem)
        .expect("Sleigh::new");
    // A second view of the same image, for the optimiser's rodata loads.
    let rom: Box<dyn strider_orchestrator::opt::ReadOnlyMemory> =
        Box::new(strider_reader::ElfFileMemReader::from_object(&obj).expect("rom reader"));
    let regs = sleigh.regs().expect("regs");
    let cc = arch.cc().build(&regs).expect("build cc");
    let mut strider =
        strider_orchestrator::Strider::new(sleigh_arch, sleigh, Some(rom)).expect("Strider::new");
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
#[ignore = "mips64-le PIC: GOT-indirect dispatch unresolved; table values lift as Add(Load[gp+off], const), not raw IntConst, and the resolver has no GOT-indirect arm yet"]
fn indirect_branch_resolved_mips64le() {
    assert_indirect_goto_resolves_to_both_labels(Arch::Mips64le);
}
#[test]
#[ignore = "mips64-be PIC: GOT-indirect dispatch unresolved; table values lift as Add(Load[gp+off], const), not raw IntConst, and the resolver has no GOT-indirect arm yet"]
fn indirect_branch_resolved_mips64be() {
    assert_indirect_goto_resolves_to_both_labels(Arch::Mips64be);
}
#[test]
fn indirect_branch_resolved_ppc32be() {
    assert_indirect_goto_resolves_to_both_labels(Arch::Ppc32be);
}
#[test]
#[ignore = "ppc32-le: stack-array dispatch unresolved; the lifter shape is uncharacterised and needs a one-shot pcode trace to identify which classifier arm is missing"]
fn indirect_branch_resolved_ppc32le() {
    assert_indirect_goto_resolves_to_both_labels(Arch::Ppc32le);
}
#[test]
#[ignore = "ppc64-be: stack-array dispatch unresolved; the lifter shape is uncharacterised and needs a one-shot pcode trace to identify which classifier arm is missing"]
fn indirect_branch_resolved_ppc64be() {
    assert_indirect_goto_resolves_to_both_labels(Arch::Ppc64be);
}
#[test]
#[ignore = "ppc64-le: stack-array dispatch unresolved; the lifter shape is uncharacterised and needs a one-shot pcode trace to identify which classifier arm is missing"]
fn indirect_branch_resolved_ppc64le() {
    assert_indirect_goto_resolves_to_both_labels(Arch::Ppc64le);
}
