//! Integration coverage for the typed `rewrite_rule` path.
//!
//! A wildcard RHS is a COMPILE error, not a runtime check: `Any` does not
//! implement `TemplatePat`. The rules below exercise the firing path.

#![allow(
    clippy::panic,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::unreachable
)]

use strider_ir::node::{NodeKind, ValueType as T};
use strider_ir::{IRBuilderExt, IRViewer, IRWalker, IntBinaryOp};
use strider_ir_test_utils::{make_empty_fn, make_fn_with_var, reg_vn};

use strider_opt::{EditFunction, apply_rules_count, rewrite_rule, rewrite_rule_runtime};
use strider_pattern::{
    Capture, CaptureExt, MatchPat, Matcher, TemplatePat, add, any_int_const, int_const,
    int_const_with, template, var,
};

// compile-fail, uncomment to re-confirm the wildcard-RHS rejection:
//
//     let rule = rewrite_rule(add(var(x), int_const(0u128)), any());
//     // error[E0277]: the trait bound `Any: TemplatePat` is not satisfied

/// `add(var(x), int_const(0)) -> var(x)` fires and redirects uses.
#[test]
fn add_zero_identity_fires_and_redirects() {
    let x = Capture::new();
    let mut fx = make_empty_fn(|b| {
        let a = b.build_int_const(7u64, T::I64)?;
        let zero = b.build_int_const(0u64, T::I64)?;
        let sum = b.build_int_binary_operation(a, zero, IntBinaryOp::Add, T::I64)?;
        // A consumer of the Add so replace_all_uses has something to do.
        b.build_int_binary_operation(sum, a, IntBinaryOp::Or, T::I64)
    })
    .unwrap();

    let rule = rewrite_rule(add(var(x), int_const(0u128)), var(x));

    let add_root = {
        let m = Matcher::new(&fx);
        let pat = add(var(x), int_const(0u128)).into_pattern();
        let hits = m.find_all(&pat).unwrap();
        assert_eq!(hits.len(), 1);
        hits[0].root()
    };

    let mut ctx = EditFunction::new(&mut fx);
    let fired = rule(&mut ctx, add_root).unwrap().is_some();
    assert!(fired, "add-zero identity should fire");

    // The Or consumer now reads the IntConst(7) twice; no Add(_, 0) is
    // reachable from any live consumer's first operand.
    let or_reads_const = ctx
        .function()
        .node_inputs(or_node(ctx.function()))
        .into_iter()
        .map(|inp| ctx.function().producer(inp))
        .all(|n| {
            let f = ctx.function();
            matches!(f.node_kind(n), NodeKind::IntConst(_))
                && f.int_const_u128(f.node_outputs(n)[0]) == Some(7)
        });
    assert!(or_reads_const, "Or should now read the redirected constant");
}

fn or_node(f: &strider_ir::Function) -> strider_ir::node::NodeId {
    f.walk()
        .find(|&n| matches!(f.node_kind(n), NodeKind::IntBinaryOp(IntBinaryOp::Or)))
        .unwrap()
}

/// An `int_const_with!` constant-fold rule folds two captured constants.
#[test]
fn const_fold_rule_via_macro() {
    let c1 = Capture::new();
    let c2 = Capture::new();

    let mut fx = make_empty_fn(|b| {
        let a = b.build_int_const(3u64, T::I64)?;
        let k = b.build_int_const(4u64, T::I64)?;
        b.build_int_binary_operation(a, k, IntBinaryOp::Add, T::I64)
    })
    .unwrap();

    let rule = rewrite_rule(
        add(any_int_const().capture(c1), any_int_const().capture(c2)),
        int_const_with!([c1: uint, c2: uint] => c1.wrapping_add(c2)),
    );

    let add_root = {
        let m = Matcher::new(&fx);
        let pat = add(any_int_const().capture(c1), any_int_const().capture(c2)).into_pattern();
        let hits = m.find_all(&pat).unwrap();
        assert!(!hits.is_empty());
        hits[0].root()
    };

    let mut ctx = EditFunction::new(&mut fx);
    let fired = rule(&mut ctx, add_root).unwrap().is_some();
    assert!(fired);

    let has_seven = ctx.function().walk().any(|n| {
        let f = ctx.function();
        matches!(f.node_kind(n), NodeKind::IntConst(_))
            && f.int_const_u128(f.node_outputs(n)[0]) == Some(7)
    });
    assert!(has_seven, "3 + 4 should fold to IntConst(7)");
}

/// A binary op nesting a computed `int_const_with!` const is a valid
/// template RHS: `(x + 1) + 2` folds to `x + 3`.
#[test]
fn reassoc_rule_nests_computed_const_in_add() {
    let x = Capture::new();
    let c1 = Capture::new();
    let c2 = Capture::new();

    // `(x + 1) + 2` over a tracked register var.
    let (mut fx, _xval) = make_fn_with_var(reg_vn(0, 8), |b, xv| {
        let one = b.build_int_const(1u64, T::I64)?;
        let inner = b.build_int_binary_operation(xv, one, IntBinaryOp::Add, T::I64)?;
        let two = b.build_int_const(2u64, T::I64)?;
        b.build_int_binary_operation(inner, two, IntBinaryOp::Add, T::I64)
    })
    .unwrap();

    let rule = rewrite_rule(
        add(
            add(var(x), any_int_const().capture(c1)),
            any_int_const().capture(c2),
        ),
        template::add(
            var(x),
            int_const_with!([c1: uint, c2: uint] => c1.wrapping_add(c2)),
        ),
    );

    let outer_root = {
        let m = Matcher::new(&fx);
        let pat = add(
            add(var(x), any_int_const().capture(c1)),
            any_int_const().capture(c2),
        )
        .into_pattern();
        let hits = m.find_all(&pat).unwrap();
        assert!(!hits.is_empty(), "reassoc LHS should match (x + 1) + 2");
        hits[0].root()
    };

    let mut ctx = EditFunction::new(&mut fx);
    let fired = rule(&mut ctx, outer_root).unwrap().is_some();
    assert!(fired, "reassoc rule should fire on (x + 1) + 2");

    let has_three = ctx.function().walk().any(|n| {
        let f = ctx.function();
        matches!(f.node_kind(n), NodeKind::IntConst(_))
            && f.int_const_u128(f.node_outputs(n)[0]) == Some(3)
    });
    assert!(has_three, "(x + 1) + 2 should reassociate to x + 3");
}

/// `rewrite_rule` panics on an RHS capture the LHS does not bind;
/// `rewrite_rule_runtime` must surface the same rejection as an `Err`.
#[test]
fn rewrite_rule_runtime_rejects_unbound_capture_in_rhs() {
    let a = Capture::new();
    let b = Capture::new();
    // Never bound by the LHS.
    let c = Capture::new();

    let lhs = add(var(a), var(b)).into_pattern();
    let rhs = var(c).into_template();

    let err = match rewrite_rule_runtime(lhs, rhs) {
        Ok(_) => panic!("expected unbound-capture rejection, got Ok"),
        Err(e) => e,
    };
    let msg = format!("{err}");
    assert!(
        msg.contains("does not bind"),
        "expected unbound-capture error mentioning \"does not bind\", got: {msg}"
    );
}

// A wildcard cannot reach `rewrite_rule_runtime` at all: its RHS is a
// `Template`, and there is no way to seal a wildcard into one. Hence no
// runtime non-buildable-RHS rejection test.
//
// compile-fail:
//
//     let rhs: strider_pattern::Template = any().into_template();
//     // error[E0277]: the trait bound `Any: TemplatePat` is not satisfied
//
//     let p: strider_pattern::Pattern = var(c).into_pattern();
//     rewrite_rule_runtime(lhs, p);
//     // error[E0308]: expected `Template`, found `Pattern`

/// `apply_rules_count` drives the rule across every reachable node.
#[test]
fn apply_rules_count_drives_rule_across_function() {
    let x = Capture::new();
    let mut fx = make_empty_fn(|b| {
        let a = b.build_int_const(9u64, T::I64)?;
        let zero = b.build_int_const(0u64, T::I64)?;
        let sum = b.build_int_binary_operation(a, zero, IntBinaryOp::Add, T::I64)?;
        b.build_int_binary_operation(sum, a, IntBinaryOp::Or, T::I64)
    })
    .unwrap();

    let rule = rewrite_rule(add(var(x), int_const(0u128)), var(x));
    let mut ctx = EditFunction::new(&mut fx);
    let fired = apply_rules_count(&mut ctx, std::slice::from_ref(&rule)).unwrap() > 0;
    assert!(fired);
}
