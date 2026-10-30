//! `IfPat` matches ONE layout. `opt::IfCondInversion` owns the other, eagerly
//! rewriting `If(Xor(C, IntConst(1)):I1){A}{B}` to the canonical `If(C){B}{A}`
//! before any pattern runs.

use strider_ir::{Function, IRViewer};
use strider_orchestrator::opt::IfCondInversion;
use strider_pattern::*;

use super::support::shapes;

fn canonicalised_inverted() -> Function {
    let mut function = shapes::if_cmp_then_return_inverted(4);
    let r = strider_orchestrator::opt::run_one(
        &IfCondInversion::new(),
        &mut function,
        &strider_orchestrator::opt::OptCtx::new(None),
    )
    .expect("opt");
    assert!(
        r.changed(),
        "IfCondInversion must rewrite the inverted-cond If"
    );
    function
}

/// Rows of `iff` joined on its true edge to the `Region` it runs into.
fn true_edge_rows(f: &Function, iff: IfPat, t: Capture) -> Vec<JoinedMatch> {
    Matcher::new(f)
        .find_joined_constrained(
            &[
                &iff.capture_true(t).build(),
                &region().any_input(anything().capture(t)).build(),
            ],
            &[],
        )
        .unwrap()
}

fn cond_4_eq_1() -> IfPat {
    if_else().cond(int_eq(int_const(4u128), int_const(1u128)))
}

#[test]
fn cond_with_true_edge_matches_direct() {
    let f = shapes::if_cmp_then_return(4);
    assert_eq!(true_edge_rows(&f, cond_4_eq_1(), Capture::new()).len(), 1);
}

#[test]
fn inverted_cond_no_match_until_canonicalised() {
    // The inverted graph's cond is `Xor(IntEq(..), IntConst(1)):I1` with the
    // branches swapped, so a pattern asking for a bare `IntEq` cond cannot
    // match it.
    let f = shapes::if_cmp_then_return_inverted(4);
    assert!(true_edge_rows(&f, cond_4_eq_1(), Capture::new()).is_empty());
}

#[test]
fn inverted_cond_matches_after_if_cond_inversion() {
    let f = canonicalised_inverted();
    assert_eq!(true_edge_rows(&f, cond_4_eq_1(), Capture::new()).len(), 1);
}

#[test]
fn cond_mismatch_no_match_in_direct() {
    let f = shapes::if_cmp_then_return(4);
    let wrong = if_else().cond(int_eq(int_const(99u128), int_const(1u128)));
    assert!(true_edge_rows(&f, wrong, Capture::new()).is_empty());
}

#[test]
fn no_cond_true_edge_matches_either_fixture() {
    for f in [
        shapes::if_cmp_then_return(4),
        shapes::if_cmp_then_return_inverted(4),
    ] {
        assert_eq!(true_edge_rows(&f, if_else(), Capture::new()).len(), 1);
    }
}

#[test]
fn captured_if_node_id_works_after_canonicalisation() {
    let n = Capture::new();
    for f in [shapes::if_cmp_then_return(4), canonicalised_inverted()] {
        let rows = true_edge_rows(&f, cond_4_eq_1().capture(n), Capture::new());
        assert_eq!(rows.len(), 1);
        assert!(matches!(
            f.node_kind(rows[0][0].node(n, f.graph()).unwrap()),
            strider_ir::node::NodeKind::If
        ));
    }
}

/// One `Capture` on the cond and on the true edge's consumer must bind one
/// node. The cond's `IntCmpOp` is never that `Region`, in either layout.
#[test]
fn shared_capture_across_cond_and_consumer_must_agree() {
    let rows = |f: &Function, cond: Capture, consumer: Capture| {
        let t = Capture::new();
        Matcher::new(f)
            .find_joined_constrained(
                &[
                    &if_else().cond(var(cond)).capture_true(t).build(),
                    &region()
                        .capture(consumer)
                        .any_input(anything().capture(t))
                        .build(),
                ],
                &[],
            )
            .unwrap()
            .len()
    };
    let c = Capture::new();
    for f in [shapes::if_cmp_then_return(4), canonicalised_inverted()] {
        assert_eq!(rows(&f, c, c), 0);
        assert_eq!(rows(&f, c, Capture::new()), 1);
    }
}
