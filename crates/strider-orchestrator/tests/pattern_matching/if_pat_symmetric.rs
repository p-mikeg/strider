//! `IfPat` direct-layout-only tests, plus integration with the
//! [`opt::IfCondInversion`] canonicalisation pass.
//!
//! In an earlier iteration `IfPat` itself tried two layouts (direct +
//! inverted).  That responsibility moved to `opt::IfCondInversion`,
//! which eagerly rewrites `If(BoolNeg(C)){A}{B}` into the canonical
//! `If(C){B}{A}` so `IfPat` only ever sees one layout.  This file
//! verifies:
//!   - the direct layout matches as before;
//!   - the inverted layout no longer matches `IfPat` directly;
//!   - after `IfCondInversion` runs, the inverted layout becomes
//!     direct and `IfPat` matches it.

use strider_orchestrator::opt::IfCondInversion;
use strider_pattern::*;

use super::support::{assertions as a, shapes};

#[test]
fn cond_with_true_branch_matches_direct() {
    let function = shapes::if_cmp_then_return(4);
    let pat = if_node()
        .cond(int_eq(int_const(4u128), int_const(1u128)))
        .with_true(any().into_pattern())
        .build();
    a::matches(&function, pat, 1);
}

#[test]
fn inverted_cond_no_match_until_canonicalised() {
    // Inverted graph: cond is `Not(IntEq(...))`, branches swapped.
    // Direct-layout `IfPat` must NOT match — the cond doesn't match the
    // pattern shape (the pattern asks for `IntEq`, not `BoolNeg(IntEq)`).
    let function = shapes::if_cmp_then_return_inverted(4);
    let pat = if_node()
        .cond(int_eq(int_const(4u128), int_const(1u128)))
        .with_true(any().into_pattern())
        .build();
    a::none(&function, pat);
}

#[test]
fn inverted_cond_matches_after_if_cond_inversion() {
    // Run the `IfCondInversion` pass to canonicalise the inverted graph,
    // then verify the same direct-layout pattern matches.  This pins the
    // contract that motivated moving the symmetry into a pass.
    let mut function = shapes::if_cmp_then_return_inverted(4);
    let r = strider_orchestrator::opt::run_one(&IfCondInversion::new(), &mut function, &mut strider_orchestrator::opt::OptCtx::empty())
        .expect("opt");
    assert!(
        r.changed(),
        "IfCondInversion must rewrite the inverted-cond If"
    );

    let pat = if_node()
        .cond(int_eq(int_const(4u128), int_const(1u128)))
        .with_true(any().into_pattern())
        .build();
    a::matches(&function, pat, 1);
}

// ── Cond mismatch still doesn't match ────────────────────────────────────────

#[test]
fn cond_mismatch_no_match_in_direct() {
    let function = shapes::if_cmp_then_return(4);
    let pat = if_node()
        .cond(int_eq(int_const(99u128), int_const(1u128))) // wrong constant
        .with_true(any().into_pattern())
        .build();
    a::none(&function, pat);
}

// ── No cond: matches both fixtures (direct and inverted) ─────────────────────

#[test]
fn no_cond_only_true_branch_matches_either_fixture() {
    // With no cond constraint, the matcher just looks for an `If` with a
    // consumer on its true output — both fixtures qualify.
    let g_direct = shapes::if_cmp_then_return(4);
    let g_inverted = shapes::if_cmp_then_return_inverted(4);
    let build_pat = || if_node().with_true(any().into_pattern()).build();
    a::matches(&g_direct, build_pat(), 1);
    a::matches(&g_inverted, build_pat(), 1);
}

// ── Capture sees the If node either way ──────────────────────────────────────

#[test]
fn captured_if_node_id_works_after_canonicalisation() {
    // Direct fixture: pattern matches and binds the If node id.
    let g_direct = shapes::if_cmp_then_return(4);
    let n = Capture::new();
    let build_pat = move || {
        if_node()
            .cond(int_eq(int_const(4u128), int_const(1u128)))
            .with_true(any().into_pattern())
            .capture(n)
            .build()
    };
    let m_d = a::unique(&g_direct, build_pat());
    assert!(matches!(
        g_direct.node_kind(m_d.node(n, g_direct.graph()).unwrap()),
        strider_ir::node::NodeKind::If
    ));

    // Inverted fixture: same pattern matches AFTER the canonicalisation
    // pass runs — verifying the capture also survives the in-place rewrite.
    let mut g_inverted = shapes::if_cmp_then_return_inverted(4);
    strider_orchestrator::opt::run_one(&IfCondInversion::new(), &mut g_inverted, &mut strider_orchestrator::opt::OptCtx::empty())
        .expect("opt");
    let m_i = a::unique(&g_inverted, build_pat());
    assert!(matches!(
        g_inverted.node_kind(m_i.node(n, g_inverted.graph()).unwrap()),
        strider_ir::node::NodeKind::If
    ));
}

// ── Shared Capture across cond and branch must agree ────────────────────────

/// A `Capture` referenced both by `cond` and by `true_branch` must bind
/// to the same node — `Bindings::bind_capture` rejects re-binds with a
/// different node id.  Pre-pass: cond's IntCmpOp and branch's consumer
/// disagree, so no match.  Post-pass (canonicalised): same disagreement,
/// still no match.  The test pins the constraint regardless of layout
/// to guard against bind-resolution regressions.
// TODO: re-enable after strider-pattern IfPat threads captures through the
// branch-walk post_match.  Current impl evaluates the branch sub-pattern
// against a throwaway `Bindings`, so a capture shared between `cond` and
// `true_branch` never collides at match time (see IfPat::From comment in
// strider-pattern/src/builders/control.rs).
#[ignore]
#[test]
fn shared_capture_across_cond_and_branch_must_agree() {
    let g_direct = shapes::if_cmp_then_return(4);
    let mut g_inverted = shapes::if_cmp_then_return_inverted(4);
    strider_orchestrator::opt::run_one(&IfCondInversion::new(), &mut g_inverted, &mut strider_orchestrator::opt::OptCtx::empty())
        .expect("opt");
    let c = Capture::new();
    let build_pat = move || {
        if_node()
            .cond(var(c))
            .with_true(var(c).into_pattern())
            .build()
    };
    a::none(&g_direct, build_pat());
    a::none(&g_inverted, build_pat());
}
