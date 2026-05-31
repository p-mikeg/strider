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

use strider_analyze::opt::{IfCondInversion, Optimizer};
use strider_analyze::pattern::*;

use super::support::{assertions as a, shapes};

#[test]
fn cond_with_true_branch_matches_direct() {
    let function = shapes::if_cmp_then_return(4);
    let pat = if_node()
        .cond(int_eq(int_const(4u64), int_const(1u64)))
        .true_branch(any());
    a::matches(&function, pat, 1);
}

#[test]
fn inverted_cond_no_match_until_canonicalised() {
    // Inverted graph: cond is `Not(IntEq(...))`, branches swapped.
    // Direct-layout `IfPat` must NOT match — the cond doesn't match the
    // pattern shape (the pattern asks for `IntEq`, not `BoolNeg(IntEq)`).
    let function = shapes::if_cmp_then_return_inverted(4);
    let pat: Pat = if_node()
        .cond(int_eq(int_const(4u64), int_const(1u64)))
        .true_branch(any())
        .into();
    a::none(&function, pat);
}

#[test]
fn inverted_cond_matches_after_if_cond_inversion() {
    // Run the `IfCondInversion` pass to canonicalise the inverted graph,
    // then verify the same direct-layout pattern matches.  This pins the
    // contract that motivated moving the symmetry into a pass.
    let mut function = shapes::if_cmp_then_return_inverted(4);
    let entry = function.entry().expect("entry");
    let r = IfCondInversion.optimize(&mut function, entry).expect("opt");
    assert!(
        r.changed(),
        "IfCondInversion must rewrite the inverted-cond If"
    );

    let pat = if_node()
        .cond(int_eq(int_const(4u64), int_const(1u64)))
        .true_branch(any());
    a::matches(&function, pat, 1);
}

// ── Cond mismatch still doesn't match ────────────────────────────────────────

#[test]
fn cond_mismatch_no_match_in_direct() {
    let function = shapes::if_cmp_then_return(4);
    let pat = if_node()
        .cond(int_eq(int_const(99u64), int_const(1u64))) // wrong constant
        .true_branch(any());
    a::none(&function, pat);
}

// ── No cond: matches both fixtures (direct and inverted) ─────────────────────

#[test]
fn no_cond_only_true_branch_matches_either_fixture() {
    // With no cond constraint, the matcher just looks for an `If` with a
    // consumer on its true output — both fixtures qualify.
    let g_direct = shapes::if_cmp_then_return(4);
    let g_inverted = shapes::if_cmp_then_return_inverted(4);
    let pat: Pat = if_node().true_branch(any()).into();
    a::matches(&g_direct, pat.clone(), 1);
    a::matches(&g_inverted, pat, 1);
}

// ── Capture sees the If node either way ──────────────────────────────────────

#[test]
fn captured_if_node_id_works_after_canonicalisation() {
    // Direct fixture: pattern matches and binds the If node id.
    let g_direct = shapes::if_cmp_then_return(4);
    let n = Capture::new();
    let pat: Pat = if_node()
        .cond(int_eq(int_const(4u64), int_const(1u64)))
        .true_branch(any())
        .capture(n);
    let m_d = a::unique(&g_direct, pat.clone());
    assert!(matches!(
        g_direct.node_kind(m_d.node(n, &g_direct).unwrap()),
        strider_ir::node::NodeKind::If
    ));

    // Inverted fixture: same pattern matches AFTER the canonicalisation
    // pass runs — verifying the capture also survives the in-place rewrite.
    let mut g_inverted = shapes::if_cmp_then_return_inverted(4);
    let entry = g_inverted.entry().expect("entry");
    IfCondInversion
        .optimize(&mut g_inverted, entry)
        .expect("opt");
    let m_i = a::unique(&g_inverted, pat);
    assert!(matches!(
        g_inverted.node_kind(m_i.node(n, &g_inverted).unwrap()),
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
#[test]
fn shared_capture_across_cond_and_branch_must_agree() {
    let g_direct = shapes::if_cmp_then_return(4);
    let mut g_inverted = shapes::if_cmp_then_return_inverted(4);
    let entry = g_inverted.entry().expect("entry");
    IfCondInversion
        .optimize(&mut g_inverted, entry)
        .expect("opt");
    let c = Capture::new();
    let pat: Pat = if_node().cond(var(c)).true_branch(var(c)).into();
    a::none(&g_direct, pat.clone());
    a::none(&g_inverted, pat);
}
