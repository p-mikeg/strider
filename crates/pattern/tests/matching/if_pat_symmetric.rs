//! Symmetric If-pattern matching: `if_node().cond(C).true_branch(T)` should
//! also match graphs where the cond is `Not(C)` and `T` is in the false
//! branch.  Models compiler-inverted if-then-else.
//!
//! All tests use [`shapes::if_cmp_then_return`] (direct layout) and
//! [`shapes::if_cmp_then_return_inverted`] (the compiler-inverted equivalent
//! that wraps the cond in `Not(...)` and swaps the branches).  The fixture
//! op is `IntCmpOp::Equal`, so the pattern op is [`int_eq`].

use pattern::*;

use super::support::{assertions as a, shapes};

// ── True-branch swap ─────────────────────────────────────────────────────────

#[test]
fn cond_with_true_branch_matches_direct() {
    let g = shapes::if_cmp_then_return(4);
    let pat = if_node()
        .cond(int_eq(int_const(4u64), int_const(1u64)))
        .true_branch(any());
    a::matches(&g, pat, 1);
}

#[test]
fn cond_with_true_branch_matches_swapped() {
    let g = shapes::if_cmp_then_return_inverted(4);
    let pat = if_node()
        .cond(int_eq(int_const(4u64), int_const(1u64)))
        .true_branch(any());
    a::matches(&g, pat, 1);
}

// ── False-branch swap ────────────────────────────────────────────────────────

#[test]
fn cond_with_false_branch_matches_direct() {
    let g = shapes::if_cmp_then_return(4);
    let pat = if_node()
        .cond(int_eq(int_const(4u64), int_const(1u64)))
        .false_branch(any());
    a::matches(&g, pat, 1);
}

#[test]
fn cond_with_false_branch_matches_swapped() {
    let g = shapes::if_cmp_then_return_inverted(4);
    let pat = if_node()
        .cond(int_eq(int_const(4u64), int_const(1u64)))
        .false_branch(any());
    a::matches(&g, pat, 1);
}

// ── Both branches: full layout swap ──────────────────────────────────────────

#[test]
fn cond_with_both_branches_matches_direct() {
    let g = shapes::if_cmp_then_return(4);
    let pat = if_node()
        .cond(int_eq(int_const(4u64), int_const(1u64)))
        .true_branch(any())
        .false_branch(any());
    a::matches(&g, pat, 1);
}

#[test]
fn cond_with_both_branches_matches_swapped() {
    // Inverted graph encodes the same source-level program; the pattern
    // (still written from the source POV) must still match.
    let g = shapes::if_cmp_then_return_inverted(4);
    let pat = if_node()
        .cond(int_eq(int_const(4u64), int_const(1u64)))
        .true_branch(any())
        .false_branch(any());
    a::matches(&g, pat, 1);
}

// ── Cond mismatch still doesn't match ────────────────────────────────────────

#[test]
fn cond_mismatch_no_match_in_direct() {
    let g = shapes::if_cmp_then_return(4);
    let pat = if_node()
        .cond(int_eq(int_const(99u64), int_const(1u64))) // wrong constant
        .true_branch(any());
    a::none(&g, pat);
}

#[test]
fn cond_mismatch_no_match_in_swapped() {
    let g = shapes::if_cmp_then_return_inverted(4);
    let pat = if_node()
        .cond(int_eq(int_const(99u64), int_const(1u64))) // wrong constant
        .true_branch(any());
    a::none(&g, pat);
}

// ── No cond: no swap (conservative semantics) ────────────────────────────────

#[test]
fn no_cond_only_true_branch_matches_direct_only() {
    // With no cond constraint, the swap is not attempted — `true_branch(p)`
    // means literally the true branch.  This is the conservative semantics
    // documented in IfPat::true_branch.  The unconstrained pattern matches
    // both fixtures because each has a real consumer of the true output;
    // no logical equivalence is folded.
    let g_direct = shapes::if_cmp_then_return(4);
    let g_inverted = shapes::if_cmp_then_return_inverted(4);
    let pat: Pat = if_node().true_branch(any()).into();
    a::matches(&g_direct, pat.clone(), 1);
    a::matches(&g_inverted, pat, 1);
}

// ── Capture sees the same If node either way ────────────────────────────────

#[test]
fn captured_if_node_id_works_in_both_layouts() {
    let g_direct = shapes::if_cmp_then_return(4);
    let g_inverted = shapes::if_cmp_then_return_inverted(4);
    let n = Capture::new();
    let pat: Pat = if_node()
        .cond(int_eq(int_const(4u64), int_const(1u64)))
        .true_branch(any())
        .capture(n);
    let m_d = a::unique(&g_direct, pat.clone());
    let m_i = a::unique(&g_inverted, pat);
    assert!(matches!(
        g_direct.graph.node_kind(m_d.node(n).unwrap()),
        ir::node::NodeKind::If
    ));
    assert!(matches!(
        g_inverted.graph.node_kind(m_i.node(n).unwrap()),
        ir::node::NodeKind::If
    ));
}

// ── Shared Capture across cond and branch must agree ────────────────────────

/// A `Capture` referenced both by `cond` and by `true_branch` must bind
/// to the same node — `Bindings::bind_capture` rejects re-binds with a
/// different node id.  The cond's bound node is an `IntCmpOp` and the
/// branch's bound node is whatever the consumer is — they cannot agree,
/// so no match is expected in either direct or inverted graphs.
#[test]
fn shared_capture_across_cond_and_branch_must_agree() {
    let g_direct = shapes::if_cmp_then_return(4);
    let g_inverted = shapes::if_cmp_then_return_inverted(4);
    let c = Capture::new();
    let pat: Pat = if_node().cond(var(c)).true_branch(var(c)).into();
    a::none(&g_direct, pat.clone());
    a::none(&g_inverted, pat);
}
