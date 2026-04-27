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
    build_int_const_target_scenario_via_stack, build_value_phi_target_scenario,
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
