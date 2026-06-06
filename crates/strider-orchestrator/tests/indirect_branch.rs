//! Computed-goto fixture tests for the IR-level indirect-branch resolver.
//!
//! `fixtures/cases/indirect_branch.c::indirect_branch_resolved` lowers
//! the indirect goto to a load from a local stack array of label
//! addresses on every supported toolchain at every optimisation level
//! we can target — gcc/clang collapse direct constant computed-gotos
//! into straight-line `mov; ret`, so the surviving lowering is
//! always: write label addresses to the local stack array, load the
//! per-iteration target from the array, branch through the loaded
//! value.
//!
//! Resolving this lowering requires **cross-region stack-load
//! forwarding** (`StackOffsetDetect` + `LoadForward` joined
//! across the function's region graph), routed through the
//! IR-level resolver's unified table-dispatch arm
//! (`strider_orchestrator::opt::classify_table_dispatch`, SP-rooted
//! base).  Cfg-time the
//! builder defers every `BranchIndirect` via `UnresolvedIndirectBranch`;
//! the IR-level resolver gets visibility into cross-region flow +
//! `LoadForward` results and resolves the dispatch into
//! `ResolvedTargets::Multiple`.
//!
//! Consequence: x86, x86_64, AArch64, ARM (LE/BE/Thumb), and MIPS-32
//! pass end-to-end.  Seven arches keep `#[ignore]` for specific
//! lifter-shape gaps documented on each test (AArch64-BE `Or(SP,K)` +
//! `Truncate`-wrapped labels, MIPS64 PIC GOT-indirect, PPC32/64).
//! When a gap closes, the ignore can be lifted and the assertion
//! ("no `UnresolvedIndirectBranch` terminator survives") will start
//! holding without any test rewrite.
//!
//! See:
//!   - docs/superpowers/specs/2026-04-27-indirect-branch-fixedpoint-design.md
//!     (§"Future work" / cross-region stack analysis).

#![allow(
    clippy::panic,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::unreachable
)]

mod common;
use common::*;
use strider_ir::{IRViewer, IRWalker};

/// Build the CFG for `indirect_branch_resolved` with the same setup
/// `common::analyze` uses (read-only-memory threaded through the cfg
/// builder), and panic if any region still carries
/// `RegionTerminator::UnresolvedIndirectBranch` at fixed point.
///
/// Reuses `common::lift_for_pipeline` for the load-ELF /
/// Sleigh / CFG-build / `analyze_cfg` prologue so this test does not
/// drift from the canonical lift path; only diverges by inspecting
/// `unresolved_branches` on the returned `AnalyzeOutcome` and
/// classifying each one through the IR-level resolver.
fn assert_no_unresolved_indirect_branch(arch: Arch) {
    let (outcome, ana, sleigh_arch, rom_for_opt) =
        lift_for_pipeline(arch, "indirect_branch", "indirect_branch_resolved");
    let endianness = sleigh_arch.endianness();
    let unresolved = outcome.unresolved_branches.clone();
    let mut function = outcome.function;

    let mut ctx = strider_orchestrator::opt::OptCtx::with_rom(&rom_for_opt);
    if unresolved.is_empty() {
        // The fixture lifted with no indirect branch to resolve (e.g.
        // an -O? collapse to straight-line code).
        // The test's promise is "no UnresolvedIndirectBranch survives";
        // that promise holds vacuously.  Mirror common::analyze's
        // post-lift sanity by running the optimiser pipeline so any
        // pipeline regression on the placeholder code-path is caught.
        let mut p = ana.build_optimizer_pipeline();
        p.add(strider_orchestrator::opt::LoadReadOnly);
        p.run(&mut function, &mut ctx).unwrap_or_else(|e| {
            panic!(
                "optimizer pipeline (no unresolved) on {}: {e:?}",
                arch.name()
            )
        });
        return;
    }
    // Run the stable optimizer subset + LoadReadOnly so the stack-store
    // detect, KnownBits, and rodata-load resolutions run before
    // classification — same shape as the orchestrator's per-iteration
    // pre-classify pass.
    let mut p = ana.build_optimizer_pipeline();
    p.add(strider_orchestrator::opt::LoadReadOnly);
    p.run(&mut function, &mut ctx)
        .unwrap_or_else(|e| panic!("optimizer pipeline on {}: {e:?}", arch.name()));

    let lr_vn = ana.calling_convention().link_register_vn;
    let stack_vn = Some(ana.calling_convention().stack_vn);
    let rom_for_classify: &dyn strider_orchestrator::opt::ReadOnlyMemory = &rom_for_opt;
    for (anchor_addr, _placeholder) in &unresolved {
        // Mirror the orchestrator's `IndirectBranchClassify` post-pass:
        // walk every reachable `IndirectBranch` node and classify its
        // *current* slot-2 input — the live dispatch value, not the
        // lift-time one an opt pass may have `replace_all_uses`-rewired
        // away.
        let mut live_anchors: Vec<strider_ir::node::ValueId> = Vec::new();
        for n in function.walk() {
            if matches!(
                function.node_kind(n),
                strider_ir::node::NodeKind::IndirectBranch
            ) {
                let inputs: Vec<strider_ir::node::ValueId> =
                    function.node_inputs(n).into_iter().collect();
                if inputs.len() == 3 {
                    live_anchors.push(inputs[2]);
                }
            }
        }
        // If no placeholder survived, the optimizer collapsed the
        // dispatch entirely (e.g. ConstantFold proved a single target and
        // the placeholder became an ABI Return).  The test's promise holds
        // vacuously.
        if live_anchors.is_empty() {
            continue;
        }
        let mut any_resolved = false;
        let view: &strider_ir::Function =
            &function;
        let known =
            strider_orchestrator::opt::analyze_known_bits(view).expect("analyze_known_bits");
        let doms = strider_ir::control_dominators(view);
        let ranges = strider_orchestrator::opt::value_range::compute_value_ranges(view, &doms, &known);
        for live in &live_anchors {
            let resolved = strider_orchestrator::opt::classify_anchor(
                view,
                *live,
                lr_vn,
                Some(rom_for_classify),
                endianness,
                stack_vn,
                &ranges,
            );
            if resolved.is_some() {
                any_resolved = true;
                break;
            }
        }
        if !any_resolved {
            panic!(
                "indirect_branch_resolved on {} has unresolved indirect \
                 branch at {anchor_addr:?} after optimisation — neither \
                 cfg-time nor IR-level (incl. stack-array classifier arm) classified \
                 the dispatch",
                arch.name(),
            );
        }
    }
}

// One #[test] per architecture.  stack-array IR-level classifier (stack-array
// arm) covers x86 / x64 / aarch64 / arm / arm-be / arm-thumb /
// mips32le / mips32be — those tests pass without `#[ignore]`.  Seven
// archs remain ignored (aarch64be / mips64 / ppc32 / ppc64 — both
// endiannesses each), each with a focused reason naming the lifter
// quirk that keeps the stack-array classifier shape match from firing.
// Closing the remaining seven is incremental — the assertion body is
// identical across arches and does not need a rewrite when each
// arch's specific shape gap is closed.

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
#[ignore = "aarch64-be: stack-array dispatch unresolved — lifter emits Or(SP,K) instead of Add(SP,K) and wraps stored labels in Truncate; resolver matches Add(SP,K)+raw-IntConst only"]
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
#[ignore = "mips64-le PIC: GOT-indirect dispatch unresolved — table values lift as Add(Load[gp+off], const), not raw IntConst; resolver has no GOT-indirect arm yet"]
fn indirect_branch_resolved_mips64le() {
    assert_no_unresolved_indirect_branch(Arch::Mips64le);
}
#[test]
#[ignore = "mips64-be PIC: GOT-indirect dispatch unresolved — table values lift as Add(Load[gp+off], const), not raw IntConst; resolver has no GOT-indirect arm yet"]
fn indirect_branch_resolved_mips64be() {
    assert_no_unresolved_indirect_branch(Arch::Mips64be);
}
#[test]
#[ignore = "ppc32-be: stack-array dispatch unresolved — lifter shape not yet characterised; needs a one-shot pcode trace to identify which classifier arm is missing"]
fn indirect_branch_resolved_ppc32be() {
    assert_no_unresolved_indirect_branch(Arch::Ppc32be);
}
#[test]
#[ignore = "ppc32-le: stack-array dispatch unresolved — lifter shape not yet characterised; needs a one-shot pcode trace to identify which classifier arm is missing"]
fn indirect_branch_resolved_ppc32le() {
    assert_no_unresolved_indirect_branch(Arch::Ppc32le);
}
#[test]
#[ignore = "ppc64-be: stack-array dispatch unresolved — lifter shape not yet characterised; needs a one-shot pcode trace to identify which classifier arm is missing"]
fn indirect_branch_resolved_ppc64be() {
    assert_no_unresolved_indirect_branch(Arch::Ppc64be);
}
#[test]
#[ignore = "ppc64-le: stack-array dispatch unresolved — lifter shape not yet characterised; needs a one-shot pcode trace to identify which classifier arm is missing"]
fn indirect_branch_resolved_ppc64le() {
    assert_no_unresolved_indirect_branch(Arch::Ppc64le);
}
