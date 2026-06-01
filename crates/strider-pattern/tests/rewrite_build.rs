//! Integration coverage for the typed `rewrite_rule` path.
//!
//! A wildcard RHS is a **compile error**, not a runtime check:
//! `rewrite_rule(add(var(x), int_const(0)), any())` would fail to
//! compile because `Any` does not implement `TemplatePat`. The throwaway
//! `rustc` spike behind this design confirmed both halves (buildable-RHS
//! compiles, wildcard-RHS errors with `Any: TemplatePat is not
//! satisfied`); the buildable-RHS rules below exercise the firing path.

#![allow(
    clippy::panic,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::unreachable
)]

use strider_ir::node::{NodeKind, NodeOutputType as T};
use strider_ir::IntBinaryOp;
use strider_ir_test_utils::{make_empty_fn, make_fn_with_var, reg_vn};

use strider_pattern::rewrite::{
    rewrite_rule, rewrite_rule_runtime, GraphRewriteCtxExt, GraphRewriter, RewriteCtx,
};
use strider_pattern::{
    add, any_int_const, int_const, int_const_with, var, Capture, CaptureExt, MatchPat, Matcher,
    TemplatePat,
};

// compile-fail: a wildcard RHS does not implement `TemplatePat`, so the
// following does NOT compile (verified with a throwaway scratch build —
// `error[E0277]: the trait bound `Any: TemplatePat` is not satisfied`):
//
//     let rule = rewrite_rule(add(var(x), int_const(0u128)), any());
//
// Uncomment to re-confirm the compile-time wildcard-RHS rejection.

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

    // Find the Add root.
    let add_root = {
        let m = Matcher::try_new(&fx).unwrap();
        let pat = add(var(x), int_const(0u128)).into_pattern();
        let hits = m.find_all(&pat);
        assert_eq!(hits.len(), 1);
        hits[0].root()
    };

    let mut ctx = RewriteCtx::try_for_built(&mut fx).unwrap();
    let fired = rule(&mut ctx, add_root).unwrap();
    assert!(fired, "add-zero identity should fire");

    // The Or consumer now reads `a` (an IntConst(7)) twice — no Add(_, 0)
    // remains reachable from any live consumer's first operand.
    let or_reads_const = ctx
        .function_ref()
        .node_inputs(or_node(ctx.function_ref()))
        .into_iter()
        .map(|inp| ctx.function_ref().node_for_output(inp))
        .all(|n| matches!(ctx.function_ref().node_kind(n), NodeKind::IntConst(7)));
    assert!(or_reads_const, "Or should now read the redirected constant");
}

/// Locate the lone `Or` node in the fixture.
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
        let m = Matcher::try_new(&fx).unwrap();
        let pat = add(any_int_const().capture(c1), any_int_const().capture(c2)).into_pattern();
        let hits = m.find_all(&pat);
        assert!(!hits.is_empty());
        hits[0].root()
    };

    let mut ctx = RewriteCtx::try_for_built(&mut fx).unwrap();
    let fired = rule(&mut ctx, add_root).unwrap();
    assert!(fired);

    // A fresh IntConst(7) now exists.
    let has_seven = ctx
        .function_ref()
        .walk()
        .any(|n| matches!(ctx.function_ref().node_kind(n), NodeKind::IntConst(7)));
    assert!(has_seven, "3 + 4 should fold to IntConst(7)");
}

/// A reassociation rule whose RHS nests a computed `int_const_with!` const
/// inside an `add` proves a binary op nesting a `ConstWith` is a valid,
/// working template RHS (the relaxed value-op factory bounds restore this).
/// `(x + 1) + 2` folds to `x + 3`.
#[test]
fn reassoc_rule_nests_computed_const_in_add() {
    let x = Capture::new();
    let c1 = Capture::new();
    let c2 = Capture::new();

    // Fixture: `(x + 1) + 2` over a tracked register var `x`.
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
        add(
            var(x),
            int_const_with!([c1: uint, c2: uint] => c1.wrapping_add(c2)),
        ),
    );

    let outer_root = {
        let m = Matcher::try_new(&fx).unwrap();
        let pat = add(
            add(var(x), any_int_const().capture(c1)),
            any_int_const().capture(c2),
        )
        .into_pattern();
        let hits = m.find_all(&pat);
        assert!(!hits.is_empty(), "reassoc LHS should match (x + 1) + 2");
        hits[0].root()
    };

    let mut ctx = RewriteCtx::try_for_built(&mut fx).unwrap();
    let fired = rule(&mut ctx, outer_root).unwrap();
    assert!(fired, "reassoc rule should fire on (x + 1) + 2");

    // The folded constant 1 + 2 == 3 now exists in the graph.
    let has_three = ctx
        .function_ref()
        .walk()
        .any(|n| matches!(ctx.function_ref().node_kind(n), NodeKind::IntConst(3)));
    assert!(has_three, "(x + 1) + 2 should reassociate to x + 3");
}

/// The runtime (FFI) rule path rejects an RHS that references a capture
/// the LHS does not bind. The compile-time `rewrite_rule` path enforces
/// this via a `.expect`, but `rewrite_rule_runtime` (the dynamically
/// constructed FFI counterpart) must surface it as an `Err`.
///
/// Restores the dropped `rewrite_new_rejects_unbound_capture_in_rhs`
/// coverage for `check_capture_coverage`'s rejection arm.
#[test]
fn rewrite_rule_runtime_rejects_unbound_capture_in_rhs() {
    let a = Capture::new();
    let b = Capture::new();
    // A fresh capture the LHS never binds.
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

// A match-only wildcard can no longer reach `rewrite_rule_runtime` at
// all: its RHS is a `Template`, and there is no way to seal a wildcard
// into a `Template` (`Any: TemplatePat` is not implemented, so
// `any().into_template()` does not type-check; and a `Pattern` is not a
// `Template`). The old runtime non-buildable-RHS rejection test is
// therefore obsolete — type-honesty makes the bad state unrepresentable.
//
// compile-fail (confirmed with a throwaway scratch build):
//
//     let rhs: strider_pattern::Template = any().into_template();
//     // error[E0277]: the trait bound `Any: TemplatePat` is not satisfied
//
//     let p: strider_pattern::Pattern = var(c).into_pattern();
//     rewrite_rule_runtime(lhs, p);
//     // error[E0308]: expected `Template`, found `Pattern`

/// `GraphRewriter::apply` drives the rule across every reachable node.
#[test]
fn graph_rewriter_applies_rule_across_function() {
    let x = Capture::new();
    let mut fx = make_empty_fn(|b| {
        let a = b.build_int_const(9u64, T::I64)?;
        let zero = b.build_int_const(0u64, T::I64)?;
        let sum = b.build_int_binary_operation(a, zero, IntBinaryOp::Add, T::I64)?;
        b.build_int_binary_operation(sum, a, IntBinaryOp::Or, T::I64)
    })
    .unwrap();

    let rule = rewrite_rule(add(var(x), int_const(0u128)), var(x));
    let fired = fx
        .with_rewrite_ctx(|ctx| GraphRewriter::apply(ctx, &rule))
        .unwrap();
    assert!(fired);
}
