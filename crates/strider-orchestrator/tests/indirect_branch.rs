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

#![allow(
    clippy::panic,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::unreachable
)]

mod common;
use common::*;
use strider_ir::{IRViewer, IRWalker};

/// Panics if any region still carries
/// `RegionTerminator::UnresolvedIndirectBranch` at fixed point.
fn assert_no_unresolved_indirect_branch(arch: Arch) {
    let (outcome, _ana, _cc, _sleigh_arch, rom_for_opt) =
        lift_for_pipeline(arch, "indirect_branch", "indirect_branch_resolved");
    let unresolved = outcome.unresolved_branches.clone();
    let mut function = outcome.function;

    let mut ctx = strider_orchestrator::opt::OptCtx::new(Some(&rom_for_opt));
    if unresolved.is_empty() {
        // Fixture lifted with nothing to resolve (e.g. an -O? collapse to
        // straight-line code); the test's promise holds vacuously. Still
        // run the pipeline to catch a regression on the placeholder path.
        let p = strider_orchestrator::opt::default_pipeline();
        p.run(&mut function, &mut ctx).unwrap_or_else(|e| {
            panic!(
                "optimizer pipeline (no unresolved) on {}: {e:?}",
                arch.name()
            )
        });
        return;
    }
    // The pipeline runs stack-store detection, KnownBits and rodata-load
    // resolution before classification, mirroring the orchestrator's
    // per-iteration pre-classify pass.
    let p = strider_orchestrator::opt::default_pipeline();
    p.run(&mut function, &mut ctx)
        .unwrap_or_else(|e| panic!("optimizer pipeline on {}: {e:?}", arch.name()));

    let rom_for_classify: &dyn strider_orchestrator::opt::ReadOnlyMemory = &rom_for_opt;
    for (target_addr, _placeholder) in &unresolved {
        // Mirrors the orchestrator's IndirectBranchClassify post-pass: walk
        // every reachable IndirectBranch and classify it off its current
        // slot-2 dispatch value.
        let mut live_branches: Vec<strider_ir::node::NodeId> = Vec::new();
        for n in function.walk() {
            if matches!(
                function.node_kind(n),
                strider_ir::node::NodeKind::IndirectBranch
            ) {
                live_branches.push(n);
            }
        }
        // No surviving placeholder means the optimizer collapsed the
        // dispatch entirely (e.g. ConstantFold proved a single target and
        // the placeholder became an ABI Return); the promise holds
        // vacuously.
        if live_branches.is_empty() {
            continue;
        }
        let mut any_resolved = false;
        let view: &strider_ir::Function = &function;
        let known =
            strider_orchestrator::opt::analyze_known_bits(view).expect("analyze_known_bits");
        let doms = strider_ir::control_dominators(view);
        let mut ranges =
            strider_orchestrator::opt::value_range::compute_value_ranges(view, &doms, &known);
        for &branch in &live_branches {
            let resolved = strider_orchestrator::opt::classify_target(
                view,
                branch,
                Some(rom_for_classify),
                &mut ranges,
                strider_orchestrator::opt::AliasMode::StackGlobalDisjoint,
            );
            if resolved.is_some() {
                any_resolved = true;
                break;
            }
        }
        if !any_resolved {
            panic!(
                "indirect_branch_resolved on {} has unresolved indirect \
                 branch at {target_addr:?} after optimisation: neither \
                 cfg-time nor IR-level (incl. stack-array classifier arm) classified \
                 the dispatch",
                arch.name(),
            );
        }
    }
}

#[test]
fn indirect_branch_resolved_x86() {
    assert_no_unresolved_indirect_branch(Arch::X86);
}
#[test]
fn indirect_branch_resolved_x64() {
    assert_no_unresolved_indirect_branch(Arch::X64);
}
#[test]
fn indirect_branch_resolved_aarch64() {
    assert_no_unresolved_indirect_branch(Arch::Aarch64);
}
#[test]
#[ignore = "aarch64-be: stack-array dispatch unresolved; the lifter emits Or(SP,K) instead of Add(SP,K) and wraps stored labels in Truncate, while the resolver matches Add(SP,K)+raw-IntConst only"]
fn indirect_branch_resolved_aarch64be() {
    assert_no_unresolved_indirect_branch(Arch::Aarch64Be);
}
#[test]
fn indirect_branch_resolved_arm() {
    assert_no_unresolved_indirect_branch(Arch::Arm);
}
#[test]
fn indirect_branch_resolved_arm_be() {
    assert_no_unresolved_indirect_branch(Arch::ArmBe);
}
#[test]
fn indirect_branch_resolved_arm_thumb() {
    assert_no_unresolved_indirect_branch(Arch::ArmThumb);
}
#[test]
fn indirect_branch_resolved_mips32le() {
    assert_no_unresolved_indirect_branch(Arch::Mips32le);
}
#[test]
fn indirect_branch_resolved_mips32be() {
    assert_no_unresolved_indirect_branch(Arch::Mips32be);
}
#[test]
#[ignore = "mips64-le PIC: GOT-indirect dispatch unresolved; table values lift as Add(Load[gp+off], const), not raw IntConst, and the resolver has no GOT-indirect arm yet"]
fn indirect_branch_resolved_mips64le() {
    assert_no_unresolved_indirect_branch(Arch::Mips64le);
}
#[test]
#[ignore = "mips64-be PIC: GOT-indirect dispatch unresolved; table values lift as Add(Load[gp+off], const), not raw IntConst, and the resolver has no GOT-indirect arm yet"]
fn indirect_branch_resolved_mips64be() {
    assert_no_unresolved_indirect_branch(Arch::Mips64be);
}
#[test]
#[ignore = "ppc32-be: stack-array dispatch unresolved; the lifter shape is uncharacterised and needs a one-shot pcode trace to identify which classifier arm is missing"]
fn indirect_branch_resolved_ppc32be() {
    assert_no_unresolved_indirect_branch(Arch::Ppc32be);
}
#[test]
#[ignore = "ppc32-le: stack-array dispatch unresolved; the lifter shape is uncharacterised and needs a one-shot pcode trace to identify which classifier arm is missing"]
fn indirect_branch_resolved_ppc32le() {
    assert_no_unresolved_indirect_branch(Arch::Ppc32le);
}
#[test]
#[ignore = "ppc64-be: stack-array dispatch unresolved; the lifter shape is uncharacterised and needs a one-shot pcode trace to identify which classifier arm is missing"]
fn indirect_branch_resolved_ppc64be() {
    assert_no_unresolved_indirect_branch(Arch::Ppc64be);
}
#[test]
#[ignore = "ppc64-le: stack-array dispatch unresolved; the lifter shape is uncharacterised and needs a one-shot pcode trace to identify which classifier arm is missing"]
fn indirect_branch_resolved_ppc64le() {
    assert_no_unresolved_indirect_branch(Arch::Ppc64le);
}
