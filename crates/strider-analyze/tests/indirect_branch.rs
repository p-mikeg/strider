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
//! forwarding** (`StackStoreDetect` + `StackLoadForward` joined
//! across the function's region graph), routed through the
//! IR-level resolver's stack-array classifier arm
//! (`strider_analyze::opt::classify_stack_array`).  The
//! cfg-time mini-graph resolver runs `ConstantFold` + `KnownBits` on
//! a single region only and cannot prove the loaded target is one of
//! the pushed label addresses; the IR-level resolver gets visibility
//! into cross-region flow + `StackLoadForward` results and resolves
//! the dispatch into `ResolvedTargets::Multiple`.
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

#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used, clippy::unreachable)]

mod common;
use common::*;

/// Build the CFG for `indirect_branch_resolved` with the same setup
/// `common::analyze` uses (read-only-memory + link-register threaded
/// through the cfg builder), and panic if any region still carries
/// `RegionTerminator::UnresolvedIndirectBranch` at fixed point.
///
/// Reuses `common::lift_for_pipeline` for the load-ELF /
/// Sleigh / CFG-build / `analyze_cfg` prologue so this test does not
/// drift from the canonical lift path; only diverges by inspecting
/// `unresolved_branches` on the returned `AnalyzeOutcome` and
/// classifying each one through the IR-level resolver.
fn assert_no_unresolved_indirect_branch(arch: Arch) {
    let (outcome, ana, _sleigh_arch, rom_for_opt) =
        lift_for_pipeline(arch, "indirect_branch", "indirect_branch_resolved");
    let unresolved = outcome.unresolved_branches.clone();
    let mut graph = outcome.graph;

    if unresolved.is_empty() {
        // the cfg-time mini-graph resolver already resolved this fixture (e.g. -O? collapse).
        // The test's promise is "no UnresolvedIndirectBranch survives";
        // that promise holds vacuously.  Mirror common::analyze's
        // post-lift sanity by running the optimiser pipeline so any
        // pipeline regression on the placeholder code-path is caught.
        let mut p = ana.build_optimizer_pipeline();
        p.add(strider_analyze::opt::LoadReadOnly::new(rom_for_opt));
        let entry = graph.entry().unwrap();
        p.run(&mut graph, entry)
            .unwrap_or_else(|e| panic!("optimizer pipeline (no unresolved) on {}: {e:?}", arch.name()));
        return;
    }
    // Run the stable optimizer subset + LoadReadOnly so the stack-store
    // detect, KnownBits, and rodata-load resolutions run before
    // classification — same shape as the orchestrator's per-iteration
    // pre-classify pass.
    let mut p = ana.build_optimizer_pipeline();
    p.add(strider_analyze::opt::LoadReadOnly::new(rom_for_opt.clone()));
    let entry = graph.entry().unwrap();
    p.run(&mut graph, entry)
        .unwrap_or_else(|e| panic!("optimizer pipeline on {}: {e:?}", arch.name()));

    let lr_vn = ana.calling_convention().link_register_vn;
    let sp_vn = Some(ana.calling_convention().stack_ptr_vn);
    let rom_for_classify = rom_for_opt;
    for (anchor_addr, anchor_output) in &unresolved {
        // After the optimizer runs, the placeholder IndirectBranch's
        // current 3rd-input may differ from the cached `anchor_output`
        // (an opt pass can `replace_all_uses` the anchor with a folded
        // expression and leave the Load detached).  Walk every
        // reachable IndirectBranch node and use its current slot 2 as
        // the live anchor for classification.  This mirrors what the
        // orchestrator's `find_placeholder_return_for_anchor` does
        // for each per-iteration classify — but here we just consume
        // the surviving placeholder on the post-optimizer graph.
        let mut live_anchors: Vec<strider_ir::node::NodeOutputId> = Vec::new();
        for n in graph.preorder() {
            if matches!(graph.node_kind(n), strider_ir::node::NodeKind::IndirectBranch) {
                let inputs: Vec<strider_ir::node::NodeOutputId> =
                    graph.node_inputs(n).into_iter().collect();
                if inputs.len() == 3 {
                    live_anchors.push(inputs[2]);
                }
            }
        }
        // If no placeholder survived, the optimizer collapsed the
        // dispatch entirely (e.g. cfg-time resolver + ConstantFold proved a
        // single target and the placeholder became an ABI Return).
        // The test's promise holds vacuously.
        if live_anchors.is_empty() {
            // Fall back to the cached anchor_output — the classifier
            // will likely also see a non-Load-shaped producer that
            // resolves via the IntConst / InitialVar(lr) arm.
            live_anchors.push(*anchor_output);
        }
        let mut any_resolved = false;
        let view: strider_analyze::pattern::RewriteCtxView<'_> = strider_analyze::pattern::RewriteCtxView::from_built(&graph).unwrap();
        let known = strider_analyze::opt::analyze_known_bits(view)
            .expect("analyze_known_bits");
        for live in &live_anchors {
            let resolved = strider_analyze::opt::classify_anchor(
                view,
                *live,
                lr_vn,
                Some(rom_for_classify.as_ref()),
                sp_vn,
                &known,
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

