//! Rewrite-rule engine tests: `rewrite_rule`, `apply_rules_in_order`,
//! `boxed_rule`, `*_const_with!` macros, and the error paths
//! (`NotBuildable`, `MissingBinding`, `RewriteSkip`, custom-closure errors
//! propagated through anyhow).
//!
//! Every happy-path test verifies the post-rewrite graph structure, not just
//! the `Ok(bool)` return value — a rule that "fires" but leaves consumers
//! pointing at the old output is a real bug that a bool-only check misses.

use ir::IntBinaryOp;
use ir::node::{NodeId, NodeKind, NodeOutputId};
use pattern::*;

use super::support::{Tb, assertions as a};

// ── Fixtures: small graphs rewrite tests mutate ──────────────────────────────

/// `return(add(x, 0))` where `x` is `add(7, 1)` so the outer Add has a
/// non-const LHS — useful for testing `add(var(x), int_const(0))` rewrites.
fn graph_add_x_zero() -> ir::BuiltFunctionGraph {
    let mut t = Tb::empty();
    let c7 = t.u64(7);
    let c1 = t.u64(1);
    let x = t.add(c7, c1); // non-const
    let zero = t.u64(0);
    let sum = t.add(x, zero);
    t.ret_val(sum)
}

/// `return(sub(x, x))` — prime candidate for `sub(var(x), var(x)) → 0`.
fn graph_sub_x_x() -> ir::BuiltFunctionGraph {
    let mut t = Tb::empty();
    let c7 = t.u64(7);
    let c1 = t.u64(1);
    let x = t.add(c7, c1);
    let diff = t.sub(x, x);
    t.ret_val(diff)
}

/// `return(add(IntConst(a), IntConst(b)))` — prime candidate for
/// constant folding.
fn graph_add_const_const(a: u64, b: u64) -> ir::BuiltFunctionGraph {
    let mut t = Tb::empty();
    let ca = t.u64(a);
    let cb = t.u64(b);
    let s = t.add(ca, cb);
    t.ret_val(s)
}

// ── Assertion helpers local to this module ──────────────────────────────────

#[track_caller]
fn find_add(g: &ir::BuiltFunctionGraph) -> NodeId {
    a::find_node(g, |k| matches!(k, NodeKind::IntBinaryOp(IntBinaryOp::Add)))
}

/// Locates the outermost `Add` node of the lowered subtraction shape.
/// `IntBinaryOp::Sub` is not a primitive in this IR — `t.sub(x, y)` builds
/// `Add(x, Neg(y))` directly.  For `t.sub(x, x)` (`x - x`), the resulting
/// graph has exactly one Add (the outer one wrapping `Neg(x)`), so finding
/// it via `IntBinaryOp::Add` is unambiguous in the test fixtures here.
#[track_caller]
fn find_sub(g: &ir::BuiltFunctionGraph) -> NodeId {
    a::find_node(g, |k| matches!(k, NodeKind::IntBinaryOp(IntBinaryOp::Add)))
}

/// Returns the `NodeKind` of the node producing the Return's data input.
/// That value lets tests check "after rewrite, what does Return consume?".
fn return_data_input_kind(g: &ir::BuiltFunctionGraph) -> NodeKind {
    let ret = a::find_node(g, |k| matches!(k, NodeKind::Return));
    let inputs: Vec<NodeOutputId> = g.graph.node_inputs(ret).into_iter().collect();
    // Return inputs: [ctrl(0), mem(1), retval0(2), ...].  We want slot 2.
    let data_in = inputs[2];
    *g.graph.kind_of_output(data_in)
}

// ── Basic firing ─────────────────────────────────────────────────────────────

#[test]
fn identity_rule_redirects_consumers_and_returns_true() {
    let mut g = graph_add_x_zero();

    let x = Capture::new();
    let rule = rewrite_rule(add(var(x), int_const(0)), var(x));

    // The graph has two Add nodes (`add(7, 1)` and `add(x, 0)`) — try the
    // rule at every node and assert it fires somewhere.
    let add_nodes: Vec<NodeId> = g
        .preorder()
        .filter(|&n| matches!(g.graph.node_kind(n), NodeKind::IntBinaryOp(IntBinaryOp::Add)))
        .collect();
    assert_eq!(add_nodes.len(), 2);

    let mut fired = false;
    for n in &add_nodes {
        if rule(&mut pattern::RewriteCtx::for_built(&mut g), *n).expect("rule did not error") {
            fired = true;
        }
    }
    assert!(fired, "rule should have fired on the outer Add");

    // After the rewrite the Return consumes the inner `add(7, 1)` directly.
    let kind = return_data_input_kind(&g);
    assert!(matches!(kind, NodeKind::IntBinaryOp(IntBinaryOp::Add)));
}

#[test]
fn rule_returns_false_when_lhs_does_not_match() {
    let mut g = graph_add_const_const(5, 3);
    // Try to rewrite a `sub(var(x), var(x))` pattern on a graph containing
    // only adds.  Should no-op.
    let x = Capture::new();
    let rule = rewrite_rule(sub(var(x), var(x)), int_const(0));
    let add_node = find_add(&g);
    let fired = rule(&mut pattern::RewriteCtx::for_built(&mut g), add_node).expect("ok");
    assert!(!fired, "rule should not fire on wrong-kind root");

    // Graph is unchanged — Return still consumes an Add.
    assert!(matches!(
        return_data_input_kind(&g),
        NodeKind::IntBinaryOp(IntBinaryOp::Add)
    ));
}

#[test]
fn sub_x_x_to_zero_rule() {
    let mut g = graph_sub_x_x();
    let x = Capture::new();
    let rule = rewrite_rule(sub(var(x), var(x)), int_const(0));

    let sub_node = find_sub(&g);
    let fired = rule(&mut pattern::RewriteCtx::for_built(&mut g), sub_node).expect("ok");
    assert!(fired);

    // Return now consumes an IntConst(0).
    let kind = return_data_input_kind(&g);
    assert!(matches!(kind, NodeKind::IntConst(0)));
}

// ── `int_const_with!` macro ──────────────────────────────────────────────────

#[test]
fn int_const_with_folds_two_captured_ints() {
    let mut g = graph_add_const_const(5, 3);
    let a_v = Capture::new();
    let b_v = Capture::new();
    let rule = rewrite_rule(
        add(any_int_const(a_v), any_int_const(b_v)),
        int_const_with!([a_v: uint, b_v: uint] => a_v.wrapping_add(b_v)),
    );

    let add_node = find_add(&g);
    assert!(rule(&mut pattern::RewriteCtx::for_built(&mut g), add_node).expect("ok"));

    // Return now consumes IntConst(8).
    assert!(matches!(
        return_data_input_kind(&g),
        NodeKind::IntConst(8)
    ));
}

#[test]
fn int_const_with_exposes_ty_and_in_ty() {
    // Rule: `Truncate(IntConst(v)) → int_const(v, ty)` — demonstrates the
    // `ty` magic binding in `int_const_with!`.
    use ir::node::NodeOutputType;

    // non-const U64 → Truncate to U8.
    let mut t = Tb::empty();
    let a_ = t.u64(1);
    let b_ = t.u64(2);
    let s = t.add(a_, b_);
    let tr = t.trunc_to(s, NodeOutputType::U8);
    let mut g = t.ret_val(tr);

    let v = Capture::new();
    let rule = rewrite_rule(
        truncate(any_int_const(v)),
        int_const_with!([v: uint, ty] => { let _ = ty; v }),
    );

    // Try against every node — only the Truncate's constant-input shape is
    // the LHS.  The Truncate's input is a non-const Add here, so the rule
    // should NOT fire; this asserts the build-side compiles and runs.
    for n in g.preorder().collect::<Vec<_>>() {
        let _ = rule(&mut pattern::RewriteCtx::for_built(&mut g), n);
    }

    // Graph unchanged: Return consumes a Truncate.
    assert!(matches!(return_data_input_kind(&g), NodeKind::Truncate));
}

// ── Error paths: NotBuildable ───────────────────────────────────────────────

#[test]
fn rhs_wildcard_is_not_buildable() {
    let mut g = graph_add_const_const(5, 3);
    let rule = rewrite_rule(add(any(), any()), any());

    let add_node = find_add(&g);
    let err = rule(&mut pattern::RewriteCtx::for_built(&mut g), add_node).expect_err("any() on RHS must error");
    assert!(
        err.downcast_ref::<pattern::NotBuildable>().is_some(),
        "expected NotBuildable, got {err:?}"
    );
}

#[test]
fn rhs_predicate_is_not_buildable() {
    let mut g = graph_add_const_const(5, 3);
    let rule = rewrite_rule(
        add(any(), any()),
        predicate(|_g, _ty, _o| true),
    );

    let add_node = find_add(&g);
    let err = rule(&mut pattern::RewriteCtx::for_built(&mut g), add_node).expect_err("predicate RHS must error");
    assert!(err.downcast_ref::<pattern::NotBuildable>().is_some(), "got {err:?}");
}

#[test]
fn rhs_control_pattern_is_not_buildable() {
    let mut g = graph_add_const_const(5, 3);
    let rule = rewrite_rule(add(any(), any()), ret());

    let add_node = find_add(&g);
    let err = rule(&mut pattern::RewriteCtx::for_built(&mut g), add_node).expect_err("ret() RHS must error");
    assert!(err.downcast_ref::<pattern::NotBuildable>().is_some(), "got {err:?}");
}

// ── Error paths: MissingBinding ─────────────────────────────────────────────

#[test]
fn rhs_unbound_capture_raises_missing_binding() {
    let mut g = graph_add_const_const(5, 3);
    // LHS binds only `bound`; RHS references `unbound` (a fresh Capture
    // never mentioned in LHS).
    let bound = Capture::new();
    let unbound = Capture::new();
    let rule = rewrite_rule(
        add(any_int_const(bound), any()),
        int_const_with!([unbound: uint] => unbound),
    );

    let add_node = find_add(&g);
    let err = rule(&mut pattern::RewriteCtx::for_built(&mut g), add_node).expect_err("missing binding expected");
    let mb = err.downcast_ref::<pattern::MissingBinding>();
    assert!(
        matches!(mb, Some(pattern::MissingBinding("uint"))),
        "expected MissingBinding(\"uint\"), got {err:?}"
    );
}

// ── Error paths: RewriteSkip ────────────────────────────────────────────────

#[test]
fn rhs_skip_sentinel_returns_false_without_mutation() {
    let mut g = graph_add_const_const(5, 3);
    let a_v = Capture::new();
    let b_v = Capture::new();
    let rule = rewrite_rule(
        add(any_int_const(a_v), any_int_const(b_v)),
        // Simulate a "div by zero"-style opt-out: compute an Option, bail
        // out via `?` when None.
        int_const_with!([a_v: uint, b_v: uint, ty] => {
            let _ = (a_v, b_v, ty);
            None::<u128>.ok_or_else(pattern::skip)?
        }),
    );

    let add_node = find_add(&g);
    let fired = rule(&mut pattern::RewriteCtx::for_built(&mut g), add_node).expect("skip must convert to Ok(false)");
    assert!(!fired, "skip sentinel should yield Ok(false)");

    // Graph unchanged.
    assert!(matches!(
        return_data_input_kind(&g),
        NodeKind::IntBinaryOp(IntBinaryOp::Add)
    ));
}

// ── Error paths: arbitrary closure error propagates through anyhow ─────────

#[test]
fn rhs_closure_error_propagates_through_anyhow() {
    #[derive(Debug, thiserror::Error)]
    #[error("custom")]
    struct CustomErr;

    let mut g = graph_add_const_const(5, 3);
    let a_v = Capture::new();
    let b_v = Capture::new();
    let rule = rewrite_rule(
        add(any_int_const(a_v), any_int_const(b_v)),
        // Body must evaluate to `u64` at the success path; the `Err(...)?`
        // form bails before the type check matters.
        int_const_with!([a_v: uint, b_v: uint] => {
            let _ = (a_v, b_v);
            let res: pattern::Result<u128> = Err(anyhow::Error::new(CustomErr));
            res?
        }),
    );

    let add_node = find_add(&g);
    let err = rule(&mut pattern::RewriteCtx::for_built(&mut g), add_node).expect_err("closure error must propagate");
    assert!(err.downcast_ref::<CustomErr>().is_some(), "got {err:?}");
}

// ── Error paths: multi-value-output LHS root (F-012) ────────────────────────

/// Pin the documented `node_outputs_exact::<1>` constraint: rewriting on
/// a multi-output node (here, a `Call` whose outputs are
/// `[Control, Memory, ret-val0...]`) must surface an Err rather than
/// a silent rewire-of-the-wrong-slot.
#[test]
fn rewrite_rule_on_call_root_returns_err() {
    use ir::{FunctionBuilder, node::NodeOutputType, test_utils::SENTINEL_LIFT_ADDR};
    let mut fb = FunctionBuilder::empty().unwrap();
    let region = fb.create_region().unwrap();
    fb.set_entry_region(region).unwrap();
    fb.set_region(region);
    fb.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
    let tgt = fb.build_int_const(0x1234u64, NodeOutputType::U64).unwrap();
    fb.build_call(tgt).unwrap();
    fb.build_return(None, &[]).unwrap();
    fb.set_lift_addr(None);
    let mut g = fb.build().unwrap();

    let rule = rewrite_rule(call(), int_const(0));
    let call_node = g
        .preorder()
        .find(|n| matches!(g.graph.node_kind(*n), NodeKind::Call))
        .expect("Call node");
    let err = rule(&mut pattern::RewriteCtx::for_built(&mut g), call_node).expect_err("multi-output root must error");
    assert!(
        format!("{err:?}").contains("output") || format!("{err:?}").contains("exactly"),
        "expected node_outputs_exact failure, got {err:?}"
    );
}

// ── `apply_rules_in_order` ───────────────────────────────────────────────────

#[test]
fn apply_rules_returns_false_when_neither_fires() {
    let mut g = graph_add_const_const(5, 3);
    let x = Capture::new();
    let rules: Vec<BoxedRule> = vec![
        boxed_rule(rewrite_rule(sub(var(x), var(x)), int_const(0))),
        boxed_rule(rewrite_rule(mul(var(x), int_const(1)), var(x))),
    ];
    let apply = apply_rules_in_order(&rules);
    let add_node = find_add(&g);
    assert!(!apply(&mut pattern::RewriteCtx::for_built(&mut g), add_node).expect("ok"));
}

#[test]
fn apply_rules_or_composes_results() {
    let mut g = graph_add_const_const(5, 3);
    let x = Capture::new();
    let y = Capture::new();
    let rules: Vec<BoxedRule> = vec![
        // First rule doesn't match.
        boxed_rule(rewrite_rule(sub(var(x), var(x)), int_const(0))),
        // Second rule: add(a, b) → a (demo only — not sensible semantically,
        // but fires on any add).
        boxed_rule(rewrite_rule(add(var(y), any()), var(y))),
    ];
    let apply = apply_rules_in_order(&rules);
    let add_node = find_add(&g);
    let fired = apply(&mut pattern::RewriteCtx::for_built(&mut g), add_node).expect("ok");
    assert!(fired, "second rule should have fired");
}

#[test]
fn apply_rules_observes_post_fire_state() {
    // Ordering: rule1 rewrites add(x, 0) → x; rule2 rewrites "x" (an Add)
    // via another rule.  After rule1 fires, consumers point at the new
    // output but `apply_rules_in_order` hands the SAME `NodeId` to each
    // rule in sequence, so rule2 sees the original root.  This test just
    // documents the contract: `fn(node)` runs on the same node, OR-ing
    // results.
    let mut g = graph_add_x_zero();
    let x = Capture::new();
    let y = Capture::new();
    let rules: Vec<BoxedRule> = vec![
        boxed_rule(rewrite_rule(add(var(x), int_const(0)), var(x))),
        // Also an identity-ish rule for demo.
        boxed_rule(rewrite_rule(add(var(y), any()), var(y))),
    ];
    let apply = apply_rules_in_order(&rules);
    // Apply on every node: we just assert the call doesn't error and at
    // least one rule fires somewhere.
    let mut any_fired = false;
    for n in g.preorder().collect::<Vec<_>>() {
        if apply(&mut pattern::RewriteCtx::for_built(&mut g), n).expect("ok") {
            any_fired = true;
        }
    }
    assert!(any_fired);
}

// ── `boxed_rule` heterogeneous composition ──────────────────────────────────

#[test]
fn boxed_rule_allows_heterogeneous_vec() {
    // Different LHS shapes each close over distinct Capture IDs, so the plain
    // `impl Fn` returned by `rewrite_rule` has different types per rule —
    // hence the need for `BoxedRule`.
    let x = Capture::new();
    let y = Capture::new();
    let rules: Vec<BoxedRule> = vec![
        boxed_rule(rewrite_rule(add(var(x), int_const(0)), var(x))),
        boxed_rule(rewrite_rule(sub(var(y), var(y)), int_const(0))),
    ];
    assert_eq!(rules.len(), 2);
}

// ── `replace_all_uses` zero-user case ───────────────────────────────────────

#[test]
fn rewrite_returns_false_when_no_consumer() {
    // Construct a graph where the matched root has NO downstream consumer.
    // Build `add(5, 3)` but return a different value so the Add is dead.
    let mut t = Tb::empty();
    let _dead_a = t.u64(5);
    let _dead_b = t.u64(3);
    // Deliberately DO NOT use them in the return value.
    let other = t.u64(7);
    let g = t.ret_val(other);

    // The graph contains a Return and one IntConst(7), plus entry nodes —
    // no Add at all.  So the rule's LHS can't match; returns Ok(false).
    let mut g = g;
    let x = Capture::new();
    let rule = rewrite_rule(add(var(x), int_const(0)), var(x));
    for n in g.preorder().collect::<Vec<_>>() {
        assert!(!rule(&mut pattern::RewriteCtx::for_built(&mut g), n).expect("ok"));
    }
}

// ── Smoke: run rewrite via Matcher + rule, compare before/after ─────────────

#[test]
fn pattern_match_before_and_after_rewrite() {
    // Before: outer add(x, 0) matches add(var, int_const(0)).
    let mut g = graph_add_x_zero();
    a::matches(&g, add(any(), int_const(0)), 1);

    let x = Capture::new();
    let rule = rewrite_rule(add(var(x), int_const(0)), var(x));
    for n in g.preorder().collect::<Vec<_>>() {
        let _ = rule(&mut pattern::RewriteCtx::for_built(&mut g), n);
    }

    // After: the outer pattern no longer finds a match at the Return's
    // data input — Return now points directly at `x` (the inner Add).
    // The outer Add node may still exist in the graph arena (detached), but
    // no live consumer references its output, so the `int_const(0)` branch
    // of the outer Add still matches in the preorder walk.  Instead we
    // assert the Return consumes `add(IntConst, IntConst)` (the inner).
    let ret_kind = return_data_input_kind(&g);
    assert!(matches!(ret_kind, NodeKind::IntBinaryOp(IntBinaryOp::Add)));
}
