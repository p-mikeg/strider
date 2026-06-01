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
use strider_ir_test_utils::make_empty_fn;

use strider_pattern::rewrite::{rewrite_rule, GraphRewriteCtxExt, GraphRewriter, RewriteCtx};
use strider_pattern::{
    add, any_int_const, int_const, int_const_with, var, Capture, CaptureExt, MatchPat, Matcher,
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
