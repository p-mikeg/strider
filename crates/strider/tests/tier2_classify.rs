//! Integration tests for [`strider::indirect_resolve_tier2::classify_anchor`].
//!
//! Each test builds a real CFG from synthetic machine code, lifts it
//! to IR via `Strider::analyze_cfg_with_unresolved`, runs the strider
//! optimiser pipeline, then calls `classify_anchor` on the placeholder
//! anchor that was recorded at lift time.  The fixture builders live
//! in `common::tier2_helpers`.
//!
//! These tests exercise the classifier end-to-end against optimised IR
//! — i.e. the exact graph shapes the orchestrator (R3) will hand to
//! the classifier in production.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use strider::indirect_resolve_tier2::{ResolvedTargets, classify_anchor};

use common::tier2_helpers::{
    build_bx_lr_scenario, build_initial_var_target_scenario_x86_64,
    build_int_const_target_scenario_via_stack, build_pop_pc_via_stack_load_forward_scenario,
    build_push_target_pop_pc_scenario, build_value_phi_target_scenario,
};

/// Spec test #7: target VN's producer folds to `IntConst(k)` after
/// the optimiser pipeline runs → `Single(k)`.
///
/// The fixture pushes a constant onto the stack and pops it into the
/// dispatch register (`push K; pop rax; jmp *rax`).  Tier 1's
/// single-region mini-graph lacks `StackLoadForward`, so it can't
/// fold the load — it returns `None` and the cfg builder defers via
/// `UnresolvedIndirectBranch`.  The full pipeline DOES run
/// `StackStoreDetect + StackLoadForward`, which together collapse
/// the load back to the pushed constant K.  The classifier then
/// sees `IntConst(K)` and returns `Single(K)`.
#[test]
fn tier_2_int_const_to_single() {
    let (graph, anchor) = build_int_const_target_scenario_via_stack(0x0000_0123);
    let result = classify_anchor(&graph, anchor, /* link_register */ None);
    assert_eq!(result, Some(ResolvedTargets::Single(0x0000_0123)));
}

/// Spec test #8: producer is `InitialVar(lr_vn)` → `LinkRegister`.
///
/// The fixture is ARM `bx lr`; the strider pipeline lifts it as a
/// placeholder Return whose value-input is `InitialVar(lr_vn)` —
/// exactly the shape the classifier's LinkRegister arm matches.
#[test]
fn tier_2_initial_var_lr_to_link_register() {
    let (graph, anchor, lr_vn) = build_bx_lr_scenario();
    let result = classify_anchor(&graph, anchor, Some(lr_vn));
    assert_eq!(result, Some(ResolvedTargets::LinkRegister));
}

/// Negative companion to the LinkRegister arm: when the producer is
/// `InitialVar(other_vn)` (a function-entry value of a non-LR
/// register), the classifier must return `None` regardless of whether
/// a link register is supplied.  Uses x86_64 `jmp *rax` with no LR
/// configured — `InitialVar(rax)`'s VN cannot equal a `None` lr.
#[test]
fn tier_2_initial_var_non_lr_returns_none() {
    let (graph, anchor) = build_initial_var_target_scenario_x86_64();
    // No link register on x86_64; the classifier must not classify
    // `InitialVar(rax)` as LinkRegister.
    let result = classify_anchor(&graph, anchor, /* link_register */ None);
    assert_eq!(result, None);
}

/// Spec test #11: `ValuePhi(IntConst(K1), IntConst(K2))` →
/// `Multiple([K1, K2])` after sort + dedup.
///
/// The fixture uses an if/else diamond where each arm stores a
/// distinct constant at the same SP-relative slot, then loads
/// from that slot at the merge.  `StackStoreDetect +
/// StackLoadForward` collapse the merge's `Load` into a synthesised
/// `ValuePhi` whose value inputs are exactly the two stored
/// constants — the producer-shape this arm classifies.
#[test]
fn tier_2_phi_of_int_consts_to_multiple() {
    let (graph, anchor) = build_value_phi_target_scenario(&[0x1000, 0x2000]);
    let result = classify_anchor(&graph, anchor, None);
    match result {
        Some(ResolvedTargets::Multiple(ts)) => {
            // Output is sort + dedup'd by the classifier; assert the
            // canonical order so a regression that shuffles the
            // result fails the test rather than silently mismatching
            // the orchestrator's edge-set comparison (R3).
            assert_eq!(ts, vec![0x1000, 0x2000]);
        }
        other => panic!("expected Multiple([0x1000, 0x2000]); got {other:?}"),
    }
}

/// Spec test #11 (3-pred companion): same as above but with 3
/// distinct constants.  Verifies the `Multiple` arm doesn't truncate
/// or duplicate at higher arity.
#[test]
fn tier_2_phi_of_three_int_consts_to_multiple() {
    let (graph, anchor) = build_value_phi_target_scenario(&[0x3000, 0x1000, 0x2000]);
    let result = classify_anchor(&graph, anchor, None);
    match result {
        Some(ResolvedTargets::Multiple(ts)) => assert_eq!(ts, vec![0x1000, 0x2000, 0x3000]),
        other => panic!("expected Multiple([0x1000, 0x2000, 0x3000]); got {other:?}"),
    }
}

/// Spec test #9 (THE HEADLINE TEST): pcode-level
/// `tmp = load[sp]; sp += 4; bx tmp` after the stable optimiser
/// subset (incl. `StackLoadForward`) produces `InitialVar(lr_vn)`
/// for the target → `LinkRegister`.
///
/// **Soundness rationale (pinned):** this is the test that proves
/// the design closes the 4 BUG-5 ARM regressions.  The function-
/// entry pseudo-push of lr to a stack slot, followed by a load from
/// the same slot at the function exit, is the natural shape gcc
/// emits for `pop {pc}` / `ldr pc, [sp]` epilogues.  After
/// `StackStoreDetect` rewrites the push as a `StackStore` and
/// `StackLoadForward` resolves the load against that store, the
/// loaded value is **structurally identical** to `InitialVar(lr)`
/// — i.e. the function-entry value of the link register.  No
/// special-cased "load-from-sp = return" heuristic is needed; the
/// LinkRegister arm matches because the producer truly IS an
/// InitialVar(lr).
///
/// See `docs/superpowers/specs/2026-04-27-indirect-branch-fixedpoint-design.md`
/// "Why this is sound across iterations" + "Tier 2 — post-IR resolver"
/// for the full argument.
#[test]
fn tier_2_pop_pc_resolves_via_stack_load_forward_to_link_register() {
    let (graph, anchor, lr_vn) = build_pop_pc_via_stack_load_forward_scenario();
    let result = classify_anchor(&graph, anchor, Some(lr_vn));
    assert_eq!(
        result,
        Some(ResolvedTargets::LinkRegister),
        "StackLoadForward must turn pop pc's target into InitialVar(lr); \
         classifier must then recognise it as LinkRegister",
    );
}

/// Spec test #10 (THE SOUNDNESS GATE): `push 0x1000; pop pc`
/// produces a `Load(IntSub(InitialVar(sp), 4))`-shaped target
/// before the optimiser folds it.  After `StackStoreDetect` +
/// `StackLoadForward` the load resolves to the **stored constant**
/// `0x1000`, NOT `InitialVar(lr)`.  The classifier therefore
/// returns `Single(0x1000)` — a tail call — NOT `LinkRegister`.
///
/// **Why this matters (pinned):** the prior in-place heuristic
/// matched `Load[InitialVar(sp) + K]` directly and classified it
/// as a return.  Under that rule, this fixture would misclassify
/// as LinkRegister — wiring a return where the program actually
/// tail-calls 0x1000.  The test pins the soundness gate that
/// motivates routing through `StackLoadForward` instead of
/// pattern-matching on the load shape.
///
/// See `docs/superpowers/specs/2026-04-27-indirect-branch-fixedpoint-design.md`
/// "Tier 2 — post-IR resolver" for the soundness rules.
#[test]
fn tier_2_push_target_pop_pc_does_not_resolve_to_link_register() {
    let target = 0x1000u64;
    let (graph, anchor, lr_vn) = build_push_target_pop_pc_scenario(target);
    let result = classify_anchor(&graph, anchor, Some(lr_vn));
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

/// Spec test #15: opaque target produces `None`/Unresolved (no
/// error inside tier 2).  The orchestrator (R3) is responsible for
/// surfacing `UnresolvedIndirectBranch` at fixed point if every
/// iteration's classifier returns `None` for the same anchor.
///
/// We reuse the x86_64 `jmp *rax` fixture from
/// `tier_2_initial_var_non_lr_returns_none` — the producer is
/// `InitialVar(rax)` with no lr supplied, which the classifier
/// must treat as opaque (None) rather than erroring.
#[test]
fn tier_2_opaque_target_returns_none() {
    let (graph, anchor) = build_initial_var_target_scenario_x86_64();
    let result = classify_anchor(&graph, anchor, /* link_register */ None);
    assert_eq!(
        result, None,
        "opaque target must classify as None — no panic, no error, no \
         unsound classification.  The orchestrator decides at fixed point.",
    );
}
