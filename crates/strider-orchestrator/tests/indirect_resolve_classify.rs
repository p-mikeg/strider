//! Integration tests for
//! [`strider_orchestrator::opt::classify_target`].
//!
//! Builds a CFG from synthetic machine code, lifts it, runs the optimiser
//! pipeline, then classifies the placeholder target recorded at lift time.
//! Fixture builders live in `common::indirect_resolve_helpers`.
//!
//! Exercises the classifier against optimised IR: the same graph shapes
//! the orchestrator hands it in production.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use strider_cfg::ResolvedTargets;
use strider_ir::node::NodeKind;
use strider_ir::{IRViewer, IRWalker};
use strider_orchestrator::opt::value_range::compute_value_ranges;
use strider_orchestrator::opt::{AliasMode, analyze_known_bits, classify_target};

/// The fixture's sole `IndirectBranch` placeholder. `classify_target` takes
/// the branch node itself: it derives the dispatch target from the branch's
/// slot-2 input and scopes the range query to it.
fn sole_branch(f: &strider_ir::Function) -> strider_ir::node::NodeId {
    f.walk()
        .find(|&n| matches!(f.node_kind(n), NodeKind::IndirectBranch))
        .expect("fixture has an IndirectBranch placeholder")
}

fn classify_target_bare(view: &strider_ir::Function) -> anyhow::Result<Option<ResolvedTargets>> {
    let known = analyze_known_bits(view)?;
    let doms = strider_ir::control_dominators(view);
    let mut ranges = compute_value_ranges(view, &doms, &known);
    Ok(classify_target(
        view,
        sole_branch(view),
        None,
        &mut ranges,
        AliasMode::StackGlobalDisjoint,
    ))
}

use common::indirect_resolve_helpers::{
    build_bx_lr_scenario, build_initial_var_target_scenario_x86_64,
    build_int_const_target_scenario_via_stack, build_pop_pc_via_stack_load_forward_scenario,
    build_push_target_pop_pc_scenario, build_stack_array_dispatch_scenario,
};

/// `push K; pop rax; jmp *rax`: `StackOffsetDetect` + `LoadForward` collapse
/// the load back to the pushed constant, so the classifier sees
/// `IntConst(K)` and returns `Single(K)`.
#[test]
fn int_const_to_single() {
    let (function, _target) = build_int_const_target_scenario_via_stack(0x0000_0123);
    let result = classify_target_bare(&function).expect("classify");
    assert_eq!(result, Some(ResolvedTargets::Single(0x0000_0123)));
}

/// ARM `bx lr` lifts to a placeholder Return whose value-input is
/// `InitialVar(lr_vn)`, the shape the LinkRegister arm matches.
#[test]
fn initial_var_lr_to_link_register() {
    let (function, _target, _lr_vn) = build_bx_lr_scenario();
    let result = classify_target_bare(&function).expect("classify");
    assert_eq!(result, Some(ResolvedTargets::LinkRegister));
}

/// Negative companion to the LinkRegister arm: producer `InitialVar(other_vn)`
/// (a non-LR register) must classify as `None` regardless of whether a link
/// register is configured. x86_64 `jmp *rax` with no LR configured.
#[test]
fn initial_var_non_lr_returns_none() {
    let (function, _target) = build_initial_var_target_scenario_x86_64();
    let result = classify_target_bare(&function).expect("classify");
    assert_eq!(result, None);
}

/// `tmp = load[sp]; sp += 4; bx tmp` (ARM `pop {pc}`): after
/// `StackOffsetDetect` rewrites the push as a stack store and `LoadForward`
/// resolves the load against it, the loaded value is structurally
/// `InitialVar(lr)`, so the LinkRegister arm matches with no special-cased
/// "load from sp = return" heuristic. Pins the fix for the ARM `pop {pc}`
/// regressions.
///
/// Soundness across iterations rests on the classifier only ever adding
/// known targets: a later iteration cannot retract an edge an earlier one
/// seated, so the fixed point is monotone.
#[test]
fn pop_pc_resolves_via_stack_load_forward_to_link_register() {
    let (function, _target, _lr_vn) = build_pop_pc_via_stack_load_forward_scenario();
    let result = classify_target_bare(&function).expect("classify");
    assert_eq!(
        result,
        Some(ResolvedTargets::LinkRegister),
        "LoadForward must turn pop pc's target into InitialVar(lr); \
         classifier must then recognise it as LinkRegister",
    );
}

/// `push 0x1000; pop pc` lifts to `Load(IntSub(InitialVar(sp), 4))` before
/// optimisation. After `StackOffsetDetect` + `LoadForward` the load resolves
/// to the stored constant, NOT `InitialVar(lr)`, so this must classify as
/// `Single(0x1000)` (a tail call), not `LinkRegister`.
///
/// Regression: the prior in-place heuristic pattern-matched
/// `Load[InitialVar(sp) + K]` directly as a return, which misclassified this
/// shape as LinkRegister, wiring a return where the program actually
/// tail-calls. Classifying against the optimised (post-LoadForward) graph
/// instead of the raw load shape is what fixes it.
#[test]
fn push_target_pop_pc_does_not_resolve_to_link_register() {
    let target = 0x1000u64;
    let (function, _target, _lr_vn) = build_push_target_pop_pc_scenario(target);
    let result = classify_target_bare(&function).expect("classify");
    assert_eq!(
        result,
        Some(ResolvedTargets::Single(target)),
        "push K; pop pc must classify as Single(K), NOT LinkRegister; \
         that's the soundness gate that killed the prior heuristic",
    );
    // Explicit negative check too, so a regression reintroducing the unsound
    // heuristic fails with a directly-named assertion, not just an equality
    // mismatch.
    assert_ne!(result, Some(ResolvedTargets::LinkRegister));
}

// The stack-array dispatch arm (`classify_table_dispatch`, reached via
// `classify_target` for a Load/And target when an SP varnode is supplied):
// N constants at contiguous SP-relative offsets, dispatch via
// `Load[(sp + base) + (idx & MASK) * stride]`, bound = MASK + 1 via
// KnownBits. The classifier sorts its output, so assertions compare
// against the sorted target set.

/// 2 targets, base offset -16, stride 8. KnownBits bounds `idx & 1` to
/// `[0, 2)` so the arm reads exactly 2 entries.
#[test]
fn stack_array_two_targets_resolves_to_multiple() {
    let targets = [0x401190u64, 0x401180u64];
    let (function, _target, _sp) = build_stack_array_dispatch_scenario(&targets, -16, 8);
    let view: &strider_ir::Function = &function;
    let known = analyze_known_bits(view).expect("analyze_known_bits");
    let doms = strider_ir::control_dominators(view);
    let mut ranges = compute_value_ranges(view, &doms, &known);
    let result = classify_target(
        view,
        sole_branch(view),
        None,
        &mut ranges,
        AliasMode::StackGlobalDisjoint,
    );
    let mut expected = targets.to_vec();
    expected.sort_unstable();
    assert_eq!(result, Some(ResolvedTargets::Multiple(expected)));
}

/// 4 targets, wider mask (`idx & 3`), bound 4. Guards against a
/// truncate-to-2 regression the 2-target test above wouldn't catch.
#[test]
fn stack_array_four_targets_resolves_to_multiple() {
    let targets = [0x401_0a0u64, 0x401_0b0, 0x401_0c0, 0x401_0d0];
    let (function, _target, _sp) = build_stack_array_dispatch_scenario(&targets, -32, 8);
    let view: &strider_ir::Function = &function;
    let known = analyze_known_bits(view).expect("analyze_known_bits");
    let doms = strider_ir::control_dominators(view);
    let mut ranges = compute_value_ranges(view, &doms, &known);
    let result = classify_target(
        view,
        sole_branch(view),
        None,
        &mut ranges,
        AliasMode::StackGlobalDisjoint,
    );
    let mut expected = targets.to_vec();
    expected.sort_unstable();
    assert_eq!(result, Some(ResolvedTargets::Multiple(expected)));
}

/// Opaque target (`InitialVar(rax)`, no lr configured) classifies as `None`:
/// no panic, no error. The orchestrator, not the classifier, decides what
/// to do at fixed point.
#[test]
fn opaque_target_returns_none() {
    let (function, _target) = build_initial_var_target_scenario_x86_64();
    let result = classify_target_bare(&function).expect("classify");
    assert_eq!(
        result, None,
        "opaque target must classify as None — no panic, no error, no \
         unsound classification.  The orchestrator decides at fixed point.",
    );
}

/// Regression: calling `classify_target` twice on the same unchanged graph
/// must produce the same verdict. Guards against a future cache (e.g. of
/// `KnownBitsMap`) added across calls without invalidation; two calls on an
/// unchanged graph would otherwise still agree by luck.
#[test]
fn classify_target_is_idempotent_on_unchanged_graph() {
    let (function, _target) = build_int_const_target_scenario_via_stack(0x0000_0123);
    let first = classify_target_bare(&function).expect("classify #1");
    let second = classify_target_bare(&function).expect("classify #2");
    assert_eq!(
        first, second,
        "two consecutive classify_target calls on an unchanged graph must agree",
    );
    assert_eq!(first, Some(ResolvedTargets::Single(0x0000_0123)));
}

// Soundness gate: a `br x30` following a `bl` (which clobbers x30 with the
// return address) must not classify as LinkRegister at the full-function IR
// level. After the stable optimiser runs, x30's value at the branch site is
// the Call's clobber output, not `InitialVar(x30)`. The old per-region
// mini-graph resolver could not see this: it only saw one region's pcode
// and had no way to know x30 was clobbered by a prior Call elsewhere. The
// rebuild-driven full-function classifier sees the clobber directly, since
// it's a value already in the graph.

/// AArch64: `bl 0x1010` (clobbers x30 with the return address) followed by
/// `br x30`. After optimisation, x30's value at the branch site is the
/// Call's clobber output, not `InitialVar(x30)`.
fn build_lr_clobbered_by_call_scenario() -> (strider_ir::Function, strider_ir::Value, rsleigh::Vn) {
    use rsleigh::mem_readers::BufMemReader;
    use strider_cfg::{CfgOptions, MachineInsnAddr};
    use strider_orchestrator::Lifter;
    use strider_target::{CallingConvention, SleighArch};

    // AArch64 LE byte encoding:
    //   bl +0x10  (target = 0x1010)  ->  0x94000004 -> 04 00 00 94
    //   br x30                       ->  0xD61F03C0 -> C0 03 1F D6
    let base = 0x1000u64;
    let mut bytes: Vec<u8> = vec![
        0x04, 0x00, 0x00, 0x94, // bl 0x1010
        0xC0, 0x03, 0x1F, 0xD6, // br x30
    ];
    // Pad so Sleigh lookahead past br x30 doesn't trip DataUnavailErr.
    bytes.extend(std::iter::repeat_n(0x00u8, 64));

    let arch = SleighArch::aarch64();
    let reader = BufMemReader::new(bytes, base);
    let sleigh =
        rsleigh::Sleigh::new(arch.sla_spec(), arch.pspec(), reader).expect("create aarch64 sleigh");

    let mut strider = Lifter::new(arch, sleigh).expect("Lifter::new");
    let cc = CallingConvention::aarch64_aapcs64()
        .build(strider.sleigh_regs())
        .expect("build cc");
    let lr_vn = cc
        .link_register_vn
        .expect("AArch64 AAPCS has a link register");

    // cfg builder does no cfg-time indirect-branch resolution, so `br x30`
    // reaches the IR as an UnresolvedIndirectBranch placeholder.
    let cfg = strider
        .build_cfg(
            MachineInsnAddr::from(base),
            &CfgOptions::default(),
            &Default::default(),
        )
        .expect("cfg build");

    let outcome = strider.build_ir(&cfg, cc).expect("build_ir");
    let mut function = outcome.function;

    // Run the pipeline so x30's value at the branch site reflects the
    // Call's clobber output, not InitialVar.
    let p = strider_orchestrator::opt::default_pipeline();
    p.run(
        &mut function,
        &mut strider_orchestrator::opt::OptCtx::new(None),
    )
    .expect("optimizer pipeline");

    assert_eq!(
        outcome.unresolved_branches.len(),
        1,
        "lr-clobber fixture must have exactly one IR-level placeholder (the br x30)",
    );

    let target = common::indirect_resolve_helpers::orchestrator::target_value_input(&function)
        .expect("lr-clobber fixture must have one IndirectBranch placeholder after optimisation");
    (function, target, lr_vn)
}

/// Soundness gate: a `br x30` after a `bl` (which clobbers x30) must not
/// classify as LinkRegister. `bl 0x1010` from 0x1000 writes the return
/// address 0x1004 into x30; after ConstantFold the Call's x30 clobber
/// resolves to `IntConst(0x1004)`, so the branch must classify as
/// `Single(0x1004)` (the literal return address), not LinkRegister.
#[test]
fn bx_lr_after_call_does_not_classify_as_link_register() {
    let (function, _target, _lr_vn) = build_lr_clobbered_by_call_scenario();
    let result = classify_target_bare(&function).expect("classify");
    assert_ne!(
        result,
        Some(ResolvedTargets::LinkRegister),
        "br x30 after a bl (which clobbers x30) must NOT classify as \
         LinkRegister — x30 holds the bl's return address, not InitialVar(lr)",
    );
    assert_eq!(
        result,
        Some(ResolvedTargets::Single(0x1004)),
        "classify_target must resolve br x30 to Single(0x1004) — the literal \
         return address that bl wrote into x30",
    );
}
