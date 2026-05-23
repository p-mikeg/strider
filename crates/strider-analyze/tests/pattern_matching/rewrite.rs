//! Rewrite-rule engine tests: `rewrite_rule`, `boxed_rule`, and the
//! error paths (`NotBuildable`, `MissingBinding`, `RewriteSkip`) surfaced
//! via the public anyhow surface and the rule's `Ok(bool)` contract.
//!
//! Tests that depend on the `int_const_with!` macro or
//! `apply_rules_in_order` are intentionally omitted — both are
//! `pub(crate)` on the current branch, so the integration suite can't
//! reach them.  Their behaviour is exercised end-to-end via
//! `tests/graph_rewriter.rs` + `tests/optimizer_pipeline_subsets.rs`.

use strider_analyze::pattern::*;
use strider_ir::IntBinaryOp;
use strider_ir::node::{NodeId, NodeKind, NodeOutputId};

use super::support::{Tb, assertions as a};

// ── Fixtures: small graphs rewrite tests mutate ──────────────────────────────

/// `return(add(x, 0))` where `x` is `add(7, 1)` so the outer Add has a
/// non-const LHS — useful for testing `add(var(x), int_const(0))` rewrites.
fn graph_add_x_zero() -> strider_ir::Graph {
    let mut t = Tb::empty();
    let c7 = t.u64(7);
    let c1 = t.u64(1);
    let x = t.add(c7, c1);
    let zero = t.u64(0);
    let sum = t.add(x, zero);
    t.ret_val(sum)
}

/// `return(sub(x, x))` — prime candidate for `sub(var(x), var(x)) → 0`.
fn graph_sub_x_x() -> strider_ir::Graph {
    let mut t = Tb::empty();
    let c7 = t.u64(7);
    let c1 = t.u64(1);
    let x = t.add(c7, c1);
    let diff = t.sub(x, x);
    t.ret_val(diff)
}

/// `return(add(IntConst(a), IntConst(b)))` — prime candidate for
/// constant folding.
fn graph_add_const_const(a: u64, b: u64) -> strider_ir::Graph {
    let mut t = Tb::empty();
    let ca = t.u64(a);
    let cb = t.u64(b);
    let s = t.add(ca, cb);
    t.ret_val(s)
}

// ── Assertion helpers local to this module ──────────────────────────────────

#[track_caller]
fn find_add(g: &strider_ir::Graph) -> NodeId {
    a::find_node(g, |k| matches!(k, NodeKind::IntBinaryOp(IntBinaryOp::Add)))
}

#[track_caller]
fn find_sub(g: &strider_ir::Graph) -> NodeId {
    a::find_node(g, |k| matches!(k, NodeKind::IntBinaryOp(IntBinaryOp::Add)))
}

/// Returns the `NodeKind` of the node producing the Return's data input.
fn return_data_input_kind(g: &strider_ir::Graph) -> NodeKind {
    let ret = a::find_node(g, |k| matches!(k, NodeKind::Return));
    let inputs: Vec<NodeOutputId> = g.node_inputs(ret).into_iter().collect();
    // Return inputs: [ctrl(0), mem(1), retval0(2), ...].
    let data_in = inputs[2];
    *g.kind_of_output(data_in)
}

/// Helper: run rule on every node, OR-ing results.
fn fire_anywhere<F>(g: &mut strider_ir::Graph, rule: F) -> bool
where
    F: Fn(&mut RewriteCtx<'_>, NodeId) -> Result<bool>,
{
    let nodes: Vec<NodeId> = g.preorder().collect();
    g.with_rewrite_ctx(|ctx| {
        let mut any = false;
        for n in nodes {
            if rule(ctx, n)? {
                any = true;
            }
        }
        Ok(any)
    })
    .expect("test fixture is built")
}

// ── Basic firing ─────────────────────────────────────────────────────────────

#[test]
fn identity_rule_redirects_consumers_and_returns_true() {
    let mut g = graph_add_x_zero();
    let x = Capture::new();
    let rule = rewrite_rule(add(var(x), int_const(0u64)), var(x));

    let fired = fire_anywhere(&mut g, rule);
    assert!(fired, "rule should have fired on the outer Add");

    // After the rewrite the Return consumes the inner `add(7, 1)` directly.
    let kind = return_data_input_kind(&g);
    assert!(matches!(kind, NodeKind::IntBinaryOp(IntBinaryOp::Add)));
}

#[test]
fn rule_returns_false_when_lhs_does_not_match() {
    let mut g = graph_add_const_const(5, 3);
    let x = Capture::new();
    let rule = rewrite_rule(sub(var(x), var(x)), int_const(0u64));
    let add_node = find_add(&g);
    let fired = g
        .with_rewrite_ctx(|ctx| rule(ctx, add_node))
        .expect("test fixture is built");
    assert!(!fired);

    assert!(matches!(
        return_data_input_kind(&g),
        NodeKind::IntBinaryOp(IntBinaryOp::Add)
    ));
}

#[test]
fn sub_x_x_to_zero_rule() {
    let mut g = graph_sub_x_x();
    let x = Capture::new();
    let rule = rewrite_rule(sub(var(x), var(x)), int_const(0u64));

    let sub_node = find_sub(&g);
    let fired = g
        .with_rewrite_ctx(|ctx| rule(ctx, sub_node))
        .expect("test fixture is built");
    assert!(fired);

    let kind = return_data_input_kind(&g);
    assert!(matches!(kind, NodeKind::IntConst(0)));
}

// ── Error paths: NotBuildable (asserted via error message) ──────────────────

fn is_not_buildable_err(err: &anyhow::Error) -> bool {
    format!("{err}").contains("not buildable")
}

#[test]
fn rhs_wildcard_is_not_buildable() {
    let mut g = graph_add_const_const(5, 3);
    let rule = rewrite_rule(add(any(), any()), any());
    let add_node = find_add(&g);
    let err = g
        .with_rewrite_ctx(|ctx| match rule(ctx, add_node) {
            Ok(_) => panic!("any() on RHS must error"),
            Err(e) => Ok(e),
        })
        .expect("test fixture is built");
    assert!(is_not_buildable_err(&err), "expected not-buildable, got {err}");
}

#[test]
fn rhs_predicate_is_not_buildable() {
    let mut g = graph_add_const_const(5, 3);
    let rule = rewrite_rule(add(any(), any()), predicate(|_g, _ty, _o| true));
    let add_node = find_add(&g);
    let err = g
        .with_rewrite_ctx(|ctx| match rule(ctx, add_node) {
            Ok(_) => panic!("predicate RHS must error"),
            Err(e) => Ok(e),
        })
        .expect("test fixture is built");
    assert!(is_not_buildable_err(&err), "got {err}");
}

#[test]
fn rhs_control_pattern_is_not_buildable() {
    let mut g = graph_add_const_const(5, 3);
    let rule = rewrite_rule(add(any(), any()), ret());
    let add_node = find_add(&g);
    let err = g
        .with_rewrite_ctx(|ctx| match rule(ctx, add_node) {
            Ok(_) => panic!("ret() RHS must error"),
            Err(e) => Ok(e),
        })
        .expect("test fixture is built");
    assert!(is_not_buildable_err(&err), "got {err}");
}

// ── Error paths: multi-value-output LHS root ────────────────────────────────

/// Pin the documented `node_outputs_exact::<1>` constraint: rewriting on
/// a multi-output node (a `Call` whose outputs are
/// `[Control, Memory, ret-val0...]`) must surface an Err rather than
/// a silent rewire-of-the-wrong-slot.
#[test]
fn rewrite_rule_on_call_root_returns_err() {
    use strider_ir::FunctionBuilder;
    use strider_ir::node::NodeOutputType;
    use strider_ir_test_utils::SENTINEL_LIFT_ADDR;
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

    let rule = rewrite_rule(call(), int_const(0u64));
    let call_node = g
        .preorder()
        .find(|n| matches!(g.node_kind(*n), NodeKind::Call))
        .expect("Call node");
    let err = g
        .with_rewrite_ctx(|ctx| match rule(ctx, call_node) {
            Ok(_) => panic!("multi-output root must error"),
            Err(e) => Ok(e),
        })
        .expect("test fixture is built");
    let dbg = format!("{err:?}");
    assert!(
        dbg.contains("output") || dbg.contains("exactly"),
        "expected node_outputs_exact failure, got {err:?}"
    );
}

// ── boxed_rule heterogeneous composition ───────────────────────────────────

#[test]
fn boxed_rule_allows_heterogeneous_vec() {
    let x = Capture::new();
    let y = Capture::new();
    let rules: Vec<BoxedRule> = vec![
        boxed_rule(rewrite_rule(add(var(x), int_const(0u64)), var(x))),
        boxed_rule(rewrite_rule(sub(var(y), var(y)), int_const(0u64))),
    ];
    assert_eq!(rules.len(), 2);
}

// ── replace_all_uses zero-user case ───────────────────────────────────────

#[test]
fn rewrite_returns_false_when_no_consumer() {
    let mut t = Tb::empty();
    let _dead_a = t.u64(5);
    let _dead_b = t.u64(3);
    let other = t.u64(7);
    let mut g = t.ret_val(other);

    let x = Capture::new();
    let rule = rewrite_rule(add(var(x), int_const(0u64)), var(x));
    let fired = fire_anywhere(&mut g, rule);
    assert!(!fired);
}

// ── Smoke: run rewrite via Matcher + rule, compare before/after ─────────────

#[test]
fn pattern_match_before_and_after_rewrite() {
    let mut g = graph_add_x_zero();
    a::matches(&g, add(any(), int_const(0u64)), 1);

    let x = Capture::new();
    let rule = rewrite_rule(add(var(x), int_const(0u64)), var(x));
    fire_anywhere(&mut g, rule);

    let ret_kind = return_data_input_kind(&g);
    assert!(matches!(ret_kind, NodeKind::IntBinaryOp(IntBinaryOp::Add)));
}
