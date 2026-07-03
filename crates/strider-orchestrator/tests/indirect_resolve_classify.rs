//! Integration tests for
//! [`strider_orchestrator::opt::classify_anchor`].
//!
//! Each test builds a real CFG from synthetic machine code, lifts it
//! to IR via `Lifter::build_ir` (which returns an `LiftOutcome`
//! carrying the `unresolved_branches` placeholder list), runs the
//! strider optimiser pipeline, then calls `classify_anchor` on the
//! placeholder anchor that was recorded at lift time.  The fixture
//! builders live in `common::indirect_resolve_helpers`.
//!
//! These tests exercise the classifier end-to-end against optimised IR
//! — i.e. the exact graph shapes the orchestrator hands to the
//! classifier in production.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use strider_cfg::ResolvedTargets;
use strider_ir::node::NodeKind;
use strider_ir::{IRViewer, IRWalker};
use strider_orchestrator::opt::value_range::compute_value_ranges;
use strider_orchestrator::opt::{AliasMode, analyze_known_bits, classify_anchor};

/// The fixture's sole `IndirectBranch` placeholder — the node `classify_anchor`
/// now takes (it derives the dispatch anchor from the branch's slot-2 input and
/// scopes the range query to it).
fn sole_branch(f: &strider_ir::Function) -> strider_ir::node::NodeId {
    f.walk()
        .find(|&n| matches!(f.node_kind(n), NodeKind::IndirectBranch))
        .expect("fixture has an IndirectBranch placeholder")
}

/// Test helper: recomputes `analyze_known_bits`, dominators, and ranges,
/// then calls `classify_anchor` with no rom.
fn classify_anchor_bare(view: &strider_ir::Function) -> anyhow::Result<Option<ResolvedTargets>> {
    let known = analyze_known_bits(view)?;
    let doms = strider_ir::control_dominators(view);
    let mut ranges = compute_value_ranges(view, &doms, &known);
    Ok(classify_anchor(
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

/// Spec test #7: target VN's producer folds to `IntConst(k)` after
/// the optimiser pipeline runs → `Single(k)`.
///
/// The fixture pushes a constant onto the stack and pops it into the
/// dispatch register (`push K; pop rax; jmp *rax`).  The cfg builder
/// defers the `jmp *rax` via `UnresolvedIndirectBranch`.  The full
/// pipeline runs `StackOffsetDetect + LoadForward`, which together
/// collapse the load back to the pushed constant K.  The classifier
/// then sees `IntConst(K)` and returns `Single(K)`.
#[test]
fn int_const_to_single() {
    let (function, _anchor) = build_int_const_target_scenario_via_stack(0x0000_0123);
    let result = classify_anchor_bare(&function).expect("classify");
    assert_eq!(result, Some(ResolvedTargets::Single(0x0000_0123)));
}

/// Spec test #8: producer is `InitialVar(lr_vn)` → `LinkRegister`.
///
/// The fixture is ARM `bx lr`; the strider pipeline lifts it as a
/// placeholder Return whose value-input is `InitialVar(lr_vn)` —
/// exactly the shape the classifier's LinkRegister arm matches.
#[test]
fn initial_var_lr_to_link_register() {
    let (function, _anchor, _lr_vn) = build_bx_lr_scenario();
    let result = classify_anchor_bare(&function).expect("classify");
    assert_eq!(result, Some(ResolvedTargets::LinkRegister));
}

/// Negative companion to the LinkRegister arm: when the producer is
/// `InitialVar(other_vn)` (a function-entry value of a non-LR
/// register), the classifier must return `None` regardless of whether
/// a link register is supplied.  Uses x86_64 `jmp *rax` with no LR
/// configured — `InitialVar(rax)`'s VN cannot equal a `None` lr.
#[test]
fn initial_var_non_lr_returns_none() {
    let (function, _anchor) = build_initial_var_target_scenario_x86_64();
    // No link register on x86_64; the classifier must not classify
    // `InitialVar(rax)` as LinkRegister.
    let result = classify_anchor_bare(&function).expect("classify");
    assert_eq!(result, None);
}

/// Spec test #9 (THE HEADLINE TEST): pcode-level
/// `tmp = load[sp]; sp += 4; bx tmp` after the stable optimiser
/// subset (incl. `LoadForward`) produces `InitialVar(lr_vn)`
/// for the target → `LinkRegister`.
///
/// **Soundness rationale (pinned):** this is the test that proves
/// the design closes the ARM `pop {pc}` regressions.  The function-
/// entry pseudo-push of lr to a stack slot, followed by a load from
/// the same slot at the function exit, is the natural shape gcc
/// emits for `pop {pc}` / `ldr pc, [sp]` epilogues.  After
/// `StackOffsetDetect` rewrites the push as a `StackStore` and
/// `LoadForward` resolves the load against that store, the
/// loaded value is **structurally identical** to `InitialVar(lr)`
/// — i.e. the function-entry value of the link register.  No
/// special-cased "load-from-sp = return" heuristic is needed; the
/// LinkRegister arm matches because the producer truly IS an
/// InitialVar(lr).
///
/// See `docs/superpowers/specs/2026-04-27-indirect-branch-fixedpoint-design.md`
/// "Why this is sound across iterations" + "the IR-level orchestrator resolver — post-IR resolver"
/// for the full argument.
#[test]
fn pop_pc_resolves_via_stack_load_forward_to_link_register() {
    let (function, _anchor, _lr_vn) = build_pop_pc_via_stack_load_forward_scenario();
    let result = classify_anchor_bare(&function).expect("classify");
    assert_eq!(
        result,
        Some(ResolvedTargets::LinkRegister),
        "LoadForward must turn pop pc's target into InitialVar(lr); \
         classifier must then recognise it as LinkRegister",
    );
}

/// Spec test #10 (THE SOUNDNESS GATE): `push 0x1000; pop pc`
/// produces a `Load(IntSub(InitialVar(sp), 4))`-shaped target
/// before the optimiser folds it.  After `StackOffsetDetect` +
/// `LoadForward` the load resolves to the **stored constant**
/// `0x1000`, NOT `InitialVar(lr)`.  The classifier therefore
/// returns `Single(0x1000)` — a tail call — NOT `LinkRegister`.
///
/// **Why this matters (pinned):** the prior in-place heuristic
/// matched `Load[InitialVar(sp) + K]` directly and classified it
/// as a return.  Under that rule, this fixture would misclassify
/// as LinkRegister — wiring a return where the program actually
/// tail-calls 0x1000.  The test pins the soundness gate that
/// motivates routing through `LoadForward` instead of
/// pattern-matching on the load shape.
///
/// See `docs/superpowers/specs/2026-04-27-indirect-branch-fixedpoint-design.md`
/// "the IR-level orchestrator resolver — post-IR resolver" for the soundness rules.
#[test]
fn push_target_pop_pc_does_not_resolve_to_link_register() {
    let target = 0x1000u64;
    let (function, _anchor, _lr_vn) = build_push_target_pop_pc_scenario(target);
    let result = classify_anchor_bare(&function).expect("classify");
    assert_eq!(
        result,
        Some(ResolvedTargets::Single(target)),
        "push K; pop pc must classify as Single(K), NOT LinkRegister; \
         that's the soundness gate that killed the prior heuristic",
    );
    // Doubly sure: this fixture produces a Single(K), not a
    // LinkRegister.  An equality check would have caught it
    // already, but we pin the negative shape explicitly so a
    // future refactor that accidentally reintroduces the unsound
    // heuristic gets a directly-named failure.
    assert_ne!(result, Some(ResolvedTargets::LinkRegister));
}

// ── O9 stack-array indirect-branch shape ──────────────────────────────────
//
// The unified table-dispatch arm's SP-rooted base
// (`strider_orchestrator::opt::classify_table_dispatch`) is reached via
// `classify_anchor` when the anchor is a `Load`/`And` and an SP varnode
// is supplied.  These tests pin the
// end-to-end shape: N constants stored at contiguous SP-relative
// offsets, dispatch via `Load[(sp + base) + (idx & MASK) * stride]`,
// and `bound = MASK + 1` derived via `KnownBits`.
//
// The classifier sorts the resulting target set; we assert against the
// sorted form so a deterministic-output regression fails the test
// rather than silently re-ordering the orchestrator's edge-set.

/// 2 targets, base offset -16, stride 8.  KnownBits bounds `idx & 1`
/// to `[0, 2)` so the stack-array arm reads exactly 2 entries.
#[test]
fn stack_array_two_targets_resolves_to_multiple() {
    let targets = [0x401190u64, 0x401180u64];
    let (function, _anchor, _sp) = build_stack_array_dispatch_scenario(&targets, -16, 8);
    let view: &strider_ir::Function = &function;
    let known = analyze_known_bits(view).expect("analyze_known_bits");
    let doms = strider_ir::control_dominators(view);
    let mut ranges = compute_value_ranges(view, &doms, &known);
    let result = classify_anchor(
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

/// 4 targets, base offset -32, stride 8.  Exercises a wider mask
/// (`idx & 3`) so the bound is 4.  Verifies the classifier doesn't
/// truncate beyond 2 entries (a regression that pinned only the
/// first two would slip through the 2-target test above).
#[test]
fn stack_array_four_targets_resolves_to_multiple() {
    let targets = [0x401_0a0u64, 0x401_0b0, 0x401_0c0, 0x401_0d0];
    let (function, _anchor, _sp) = build_stack_array_dispatch_scenario(&targets, -32, 8);
    let view: &strider_ir::Function = &function;
    let known = analyze_known_bits(view).expect("analyze_known_bits");
    let doms = strider_ir::control_dominators(view);
    let mut ranges = compute_value_ranges(view, &doms, &known);
    let result = classify_anchor(
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

/// Spec test #15: opaque target produces `None`/Unresolved (no
/// error inside the resolver).  The orchestrator is responsible for
/// surfacing `UnresolvedIndirectBranch` at fixed point if every
/// iteration's classifier returns `None` for the same anchor.
///
/// We reuse the x86_64 `jmp *rax` fixture from
/// `initial_var_non_lr_returns_none` — the producer is
/// `InitialVar(rax)` with no lr supplied, which the classifier
/// must treat as opaque (None) rather than erroring.
#[test]
fn opaque_target_returns_none() {
    let (function, _anchor) = build_initial_var_target_scenario_x86_64();
    let result = classify_anchor_bare(&function).expect("classify");
    assert_eq!(
        result, None,
        "opaque target must classify as None — no panic, no error, no \
         unsound classification.  The orchestrator decides at fixed point.",
    );
}

/// Regression: calling `classify_anchor` twice on the
/// same graph (without optimization between calls) must produce the
/// same verdict.  Pins the invariant that no per-call state leaks
/// between invocations — every call recomputes `analyze_known_bits`
/// from the current graph state.
///
/// Concrete failure mode this would catch: a future refactor caching
/// the `KnownBitsMap` across `classify_anchor` calls without
/// invalidating the cache when the graph changes.  Two consecutive
/// calls on an unchanged graph would still agree by luck; this test
/// pins agreement on consecutive calls so a stale-cache bug shows up
/// the moment someone adds the cache without proper invalidation.
#[test]
fn classify_anchor_is_idempotent_on_unchanged_graph() {
    let (function, _anchor) = build_int_const_target_scenario_via_stack(0x0000_0123);
    let first = classify_anchor_bare(&function).expect("classify #1");
    let second = classify_anchor_bare(&function).expect("classify #2");
    assert_eq!(
        first, second,
        "two consecutive classify_anchor calls on an unchanged graph must agree",
    );
    assert_eq!(first, Some(ResolvedTargets::Single(0x0000_0123)));
}

// ── Soundness gate: LR-clobber correctness ────────────────────────────────
//
// A `br x30` that follows a `bl <addr>` (which clobbers x30 with the return
// address) must NOT classify as `LinkRegister` at the full-function IR level.
// After the stable optimiser runs, the x30 value at the `br x30` site is the
// Call's clobber output — NOT `InitialVar(x30)` — so the classifier must
// return `None` (unresolved) rather than `LinkRegister`.
//
// This is a correctness property the old per-region mini-graph resolver could
// NOT guarantee: it only saw one region's pcode and could not know that x30
// was clobbered by a prior Call in a different (or the same) region.  The
// rebuild-driven approach classifies at the FULL-FUNCTION IR level, so the
// clobbered x30 value is already in the graph — `classify_anchor` naturally
// sees the Call's output rather than the function-entry value.

/// Fixture builder for the LR-clobber scenario.
///
/// AArch64 assembly (little-endian):
///   0x1000:  bl 0x1010    ; call (clobbers x30 = lr with return addr 0x1004)
///   0x1004:  br x30       ; indirect branch through x30 → placeholder
///
/// After the stable optimiser runs, the placeholder's anchor value is
/// the Call's clobber output for x30, NOT `InitialVar(x30)`.
fn build_lr_clobbered_by_call_scenario() -> (strider_ir::Function, strider_ir::Value, rsleigh::Vn) {
    use rsleigh::mem_readers::BufMemReader;
    use strider_cfg::{CfgOptions, MachineInsnAddr};
    use strider_orchestrator::Lifter;
    use strider_target::{CallingConvention, SleighArch};

    // AArch64 LE byte encoding:
    //   bl +0x10  (target = 0x1010)  →  0x94000004 → 04 00 00 94
    //   br x30                        →  0xD61F03C0 → C0 03 1F D6
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

    // The driver OWNS the Sleigh and builds the CFG itself.
    let mut strider = Lifter::new(arch, sleigh).expect("Lifter::new");
    let cc = CallingConvention::aarch64_aapcs64()
        .build(strider.sleigh_regs())
        .expect("build cc");
    let lr_vn = cc
        .link_register_vn
        .expect("AArch64 AAPCS has a link register");

    // The cfg builder does no cfg-time indirect-branch resolution, so
    // the `br x30` reaches the IR as an UnresolvedIndirectBranch
    // placeholder for the IR-level resolver to classify.
    let cfg = strider
        .build_cfg(MachineInsnAddr::from(base), &CfgOptions::default())
        .expect("cfg build");

    let outcome = strider.build_ir(&cfg, cc).expect("build_ir");
    let mut function = outcome.function;

    // Run the full optimiser pipeline so x30's value at the `br x30`
    // site reflects the Call's clobber output (not InitialVar).
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

    let anchor = common::indirect_resolve_helpers::orchestrator::anchor_value_input(&function)
        .expect("lr-clobber fixture must have one IndirectBranch placeholder after optimisation");
    (function, anchor, lr_vn)
}

/// Soundness gate: after a `bl` (which clobbers x30/lr), a `br x30`
/// must NOT classify as `LinkRegister` at the full-function IR level.
///
/// The old per-region mini-graph resolver saw only the instruction region's
/// pcode — if x30 had no write within that region's pcode, it could
/// (incorrectly) see `InitialVar(x30)` and classify as `LinkRegister` even
/// though the actual x30 value was overwritten by the preceding `bl`.
///
/// At the full-function IR level the Call's clobber for x30 is visible.
/// Here, `bl 0x1010` from address 0x1000 stores the return address 0x1004
/// in x30: after ConstantFold the Call's x30 clobber output resolves to
/// `IntConst(0x1004)`.  `classify_anchor` then returns `Single(0x1004)`,
/// correctly identifying the branch as jumping to the literal return
/// address — NOT a return to the function's caller (LinkRegister).
///
/// This pins the soundness property: the rebuild-driven full-function IR
/// classifier can NEVER produce `LinkRegister` for a branch whose LR value
/// was overwritten by an intervening Call, because the overwrite is
/// structurally visible in the IR graph.
#[test]
fn bx_lr_after_call_does_not_classify_as_link_register() {
    let (function, _anchor, _lr_vn) = build_lr_clobbered_by_call_scenario();
    let result = classify_anchor_bare(&function).expect("classify");
    // Critical invariant: must NOT be LinkRegister.
    assert_ne!(
        result,
        Some(ResolvedTargets::LinkRegister),
        "br x30 after a bl (which clobbers x30) must NOT classify as \
         LinkRegister — x30 holds the bl's return address, not InitialVar(lr)",
    );
    // Concrete expected outcome: Single(0x1004) — the return address from bl.
    // `bl 0x1010` from 0x1000 writes `0x1000 + 4 = 0x1004` into x30; after
    // ConstantFold the Call's x30 clobber resolves to IntConst(0x1004).
    assert_eq!(
        result,
        Some(ResolvedTargets::Single(0x1004)),
        "classify_anchor must resolve br x30 to Single(0x1004) — the literal \
         return address that bl wrote into x30",
    );
}
