//! `IfPat` matches ONE layout. `opt::IfCondInversion` owns the other, eagerly
//! rewriting `If(Xor(C, IntConst(1)):I1){A}{B}` to the canonical `If(C){B}{A}`
//! before any pattern runs.

use strider_ir::IRViewer;
use strider_orchestrator::opt::IfCondInversion;
use strider_pattern::*;

use super::support::{assertions as a, shapes};

#[test]
fn cond_with_true_branch_matches_direct() {
    let function = shapes::if_cmp_then_return(4);
    let pat = if_else()
        .cond(int_eq(int_const(4u128), int_const(1u128)))
        .with_true(anything().into_pattern())
        .build();
    a::matches(&function, pat, 1);
}

#[test]
fn inverted_cond_no_match_until_canonicalised() {
    // The inverted graph's cond is `Xor(IntEq(..), IntConst(1)):I1` with the
    // branches swapped, so a pattern asking for a bare `IntEq` cond cannot
    // match it.
    let function = shapes::if_cmp_then_return_inverted(4);
    let pat = if_else()
        .cond(int_eq(int_const(4u128), int_const(1u128)))
        .with_true(anything().into_pattern())
        .build();
    a::none(&function, pat);
}

#[test]
fn inverted_cond_matches_after_if_cond_inversion() {
    let mut function = shapes::if_cmp_then_return_inverted(4);
    let r = strider_orchestrator::opt::run_one(
        &IfCondInversion::new(),
        &mut function,
        &mut strider_orchestrator::opt::OptCtx::new(None),
    )
    .expect("opt");
    assert!(
        r.changed(),
        "IfCondInversion must rewrite the inverted-cond If"
    );

    let pat = if_else()
        .cond(int_eq(int_const(4u128), int_const(1u128)))
        .with_true(anything().into_pattern())
        .build();
    a::matches(&function, pat, 1);
}

#[test]
fn cond_mismatch_no_match_in_direct() {
    let function = shapes::if_cmp_then_return(4);
    let pat = if_else()
        .cond(int_eq(int_const(99u128), int_const(1u128))) // wrong constant
        .with_true(anything().into_pattern())
        .build();
    a::none(&function, pat);
}

#[test]
fn no_cond_only_true_branch_matches_either_fixture() {
    // The cond-free pattern looks for an `If` with a consumer on its true
    // output; both fixtures qualify.
    let g_direct = shapes::if_cmp_then_return(4);
    let g_inverted = shapes::if_cmp_then_return_inverted(4);
    let build_pat = || if_else().with_true(anything().into_pattern()).build();
    a::matches(&g_direct, build_pat(), 1);
    a::matches(&g_inverted, build_pat(), 1);
}

#[test]
fn captured_if_node_id_works_after_canonicalisation() {
    // Direct fixture: pattern matches and binds the If node id.
    let g_direct = shapes::if_cmp_then_return(4);
    let n = Capture::new();
    let build_pat = move || {
        if_else()
            .cond(int_eq(int_const(4u128), int_const(1u128)))
            .with_true(anything().into_pattern())
            .capture(n)
            .build()
    };
    let m_d = a::unique(&g_direct, build_pat());
    assert!(matches!(
        g_direct.node_kind(m_d.node(n, g_direct.graph()).unwrap()),
        strider_ir::node::NodeKind::If
    ));

    // Inverted fixture: same pattern matches after the canonicalisation
    // pass runs, verifying the capture also survives the in-place rewrite.
    let mut g_inverted = shapes::if_cmp_then_return_inverted(4);
    strider_orchestrator::opt::run_one(
        &IfCondInversion::new(),
        &mut g_inverted,
        &mut strider_orchestrator::opt::OptCtx::new(None),
    )
    .expect("opt");
    let m_i = a::unique(&g_inverted, build_pat());
    assert!(matches!(
        g_inverted.node_kind(m_i.node(n, g_inverted.graph()).unwrap()),
        strider_ir::node::NodeKind::If
    ));
}

/// A `Capture` referenced both by `cond` and by `true_branch` must bind
/// to the same node: `Bindings::bind_capture` rejects re-binds with a
/// different node id. Pre-pass, cond's IntCmpOp and branch's consumer
/// disagree, so no match; post-pass (canonicalised), same disagreement,
/// still no match. The constraint holds in both layouts.
#[test]
fn shared_capture_across_cond_and_branch_must_agree() {
    let g_direct = shapes::if_cmp_then_return(4);
    let mut g_inverted = shapes::if_cmp_then_return_inverted(4);
    strider_orchestrator::opt::run_one(
        &IfCondInversion::new(),
        &mut g_inverted,
        &mut strider_orchestrator::opt::OptCtx::new(None),
    )
    .expect("opt");
    let c = Capture::new();
    let build_pat = move || {
        if_else()
            .cond(var(c))
            .with_true(var(c).into_pattern())
            .build()
    };
    a::none(&g_direct, build_pat());
    a::none(&g_inverted, build_pat());
}
