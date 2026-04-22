use super::*;
use crate::error::{Error, ErrorKind, Result};
use crate::pat::{add as pat_add, int_const as pat_int_const, var as pat_var};
use crate::var::Var;
use crate::{bool_const_with, float_const_with, int_const_with};
use ir::node::{NodeId, NodeKind, NodeOutputId, NodeOutputType};
use ir::{FunctionBuilder, IntBinaryOp};

/// Build a tiny graph: `add(x, 0) + 0`, returning the outer add.
///
/// Returning the graph plus the outer-add `NodeId` so tests can fire the
/// rule directly.  We wrap in another `add(…, 1)` so the outer add has a
/// downstream consumer (the return) and `replace_all_uses` has work to do.
fn graph_add_x_plus_zero()
-> ir::Result<(ir::BuiltFunctionGraph, NodeId, NodeOutputId)> {
    use ir::IntBinaryOp;
    let mut b = FunctionBuilder::new_raw(vec![], &[], &[], &[], None, 0)?;
    let r = b.create_region()?;
    b.set_entry_region(r)?;
    b.set_region(r);
    // `x` is just a constant — the rule doesn't care, it only needs the
    // structure `add(x, 0)`.
    let x = b.build_int_const(7, NodeOutputType::U64);
    let zero = b.build_int_const(0, NodeOutputType::U64);
    let add_out =
        b.build_int_binary_operation(x, zero, IntBinaryOp::Add, NodeOutputType::U64)?;
    // A second operation consumes `add_out` so that `replace_all_uses`
    // has at least one edge to redirect.  Return it.
    b.build_return(Some(add_out), &[])?;
    let fg = b.build()?;
    // Find the Add node.
    let add_node = fg.graph.get_node_from_output(add_out);
    Ok((fg, add_node, add_out))
}

#[test]
fn rewrite_rule_identity_x_plus_zero() -> Result<()> {
    let (mut fg, add_node, _add_out) = graph_add_x_plus_zero()?;

    let x = Var::new();
    let rule = rewrite_rule(pat_add(pat_var(x), pat_int_const(0)), cap(x));

    let changed = rule(&mut fg, add_node)?;
    assert!(
        changed,
        "x + 0 => x should redirect the return's consumer of the Add output"
    );
    Ok(())
}

#[test]
fn rewrite_rule_no_match_returns_ok_false() -> Result<()> {
    let (mut fg, add_node, _add_out) = graph_add_x_plus_zero()?;

    // A rule that matches `mul(x, 1)`, which our graph doesn't have.
    use crate::pat::mul as pat_mul;
    let x = Var::new();
    let rule = rewrite_rule(pat_mul(pat_var(x), pat_int_const(1)), cap(x));

    let changed = rule(&mut fg, add_node)?;
    assert!(!changed, "rule whose LHS doesn't match should return Ok(false)");
    Ok(())
}

#[test]
fn rewrite_rule_skip_rhs_returns_ok_false() -> Result<()> {
    let (mut fg, add_node, _add_out) = graph_add_x_plus_zero()?;

    // LHS matches, but RHS is Skip → rewrite is aborted.
    let x = Var::new();
    let rule = rewrite_rule(pat_add(pat_var(x), pat_int_const(0)), skip());

    let changed = rule(&mut fg, add_node)?;
    assert!(!changed, "Build::Skip at the top level should report no change");
    Ok(())
}

#[test]
fn rewrite_rule_computed_const_failure_is_propagated() -> Result<()> {
    // Sanity-check: a closure returning an error surfaces through the
    // rewrite engine as a `pattern::Error`.
    use crate::pat::mul as pat_mul;
    let (mut fg, add_node, _) = graph_add_x_plus_zero()?;

    #[derive(Debug, thiserror::Error)]
    #[error("custom closure error")]
    struct CustomError;

    let x = Var::new();
    // LHS doesn't match this graph, so the closure never fires; we still
    // exercise the construction path.  (A positive test — LHS matches and
    // closure fires — is deferred to A4 where the `int_const_with!` macro
    // will supply the full typed-capture wiring.)
    let rule = rewrite_rule(
        pat_mul(pat_var(x), pat_int_const(1)),
        int_const_fn(|_ctx| Err(Error::rewrite_closure(CustomError))),
    );
    let changed = rule(&mut fg, add_node)?;
    assert!(!changed);
    Ok(())
}

#[test]
fn apply_rules_in_order_runs_until_one_fires() -> Result<()> {
    use crate::pat::mul as pat_mul;
    let (mut fg, add_node, _) = graph_add_x_plus_zero()?;

    let x1 = Var::new();
    let rule_no_match =
        rewrite_rule(pat_mul(pat_var(x1), pat_int_const(1)), cap(x1));
    let x2 = Var::new();
    let rule_hit =
        rewrite_rule(pat_add(pat_var(x2), pat_int_const(0)), cap(x2));

    let rules = vec![rule_no_match, rule_hit];
    let combined = apply_rules_in_order(&rules);
    let changed = combined(&mut fg, add_node)?;
    assert!(changed, "at least one rule fired");
    Ok(())
}

// ── int_const_with! / bool_const_with! / float_const_with! ────────────────

// `int_const_with!`, `bool_const_with!`, and `float_const_with!` are
// `#[macro_export]` macros and are addressed via `$crate::` inside the
// pattern crate — no extra `use` is required here.

/// Macro expansion with zero captures still compiles and produces a
/// valid `Build::IntConst`.  Fires the rule against `add(x, 0)` so we
/// can smoke-test end-to-end without relying on any other capture
/// wiring.
#[test]
fn int_const_with_zero_captures_compiles_and_runs() -> Result<()> {
    let (mut fg, add_node, _) = graph_add_x_plus_zero()?;

    let x = Var::new();
    let rule = rewrite_rule(
        pat_add(pat_var(x), pat_int_const(0)),
        int_const_with!([] => 42u64),
    );
    let changed = rule(&mut fg, add_node)?;
    assert!(changed, "rule should fire and redirect uses");
    Ok(())
}

/// Single-capture `int_const_with!` body referencing the captured
/// [`IntVar`] and the auto-bound `in_ty`.  Matches
/// `Popcount(IntConst(0b1011, U32))`, rewrites to `IntConst(3, U32)`.
#[test]
fn int_const_with_popcount_rewrite() -> Result<()> {
    use crate::pat::{any_int_const, popcount};
    use crate::var::IntVar;

    let mut b = FunctionBuilder::new_raw(vec![], &[], &[], &[], None, 0)?;
    let r = b.create_region()?;
    b.set_entry_region(r)?;
    b.set_region(r);
    let c = b.build_int_const(0b1011, NodeOutputType::U32);
    let pc_out = b.build_popcount(c, NodeOutputType::U32)?;
    b.build_return(Some(pc_out), &[])?;
    let mut fg = b.build()?;

    let pc_node = fg.graph.get_node_from_output(pc_out);

    let v = IntVar::new();
    let rule = rewrite_rule(
        popcount(any_int_const(v)),
        int_const_with!([v, in_ty] => {
            // `in_ty` is bound by the macro to `Option<NodeOutputType>`;
            // narrow it and unwrap for this test.
            let ty_in: NodeOutputType = in_ty.ok_or_else(|| {
                Error::rewrite_closure(std::io::Error::other(
                    "expected integer input type",
                ))
            })?;
            ty_in.get_unsigned_int(v).unwrap_or(0).count_ones() as u64
        }),
    );
    let changed = rule(&mut fg, pc_node)?;
    assert!(changed, "popcount rule should fire");

    // The Return's retval should now be the new IntConst(3, U32).
    // Locate it via the return node.
    let ret_node = {
        let mut found = None;
        for n in fg.preorder() {
            if matches!(fg.graph.node_kind(n), NodeKind::Return) {
                found = Some(n);
                break;
            }
        }
        found.ok_or_else(|| ErrorKind::AssertionFailed("no Return node".into()))?
    };
    let ret_inputs: Vec<NodeOutputId> =
        fg.graph.node_inputs(ret_node).into_iter().collect();
    // Return inputs = [ctrl(0), retval0(1), …]
    let retval = ret_inputs.get(1).copied().ok_or_else(|| {
        ErrorKind::AssertionFailed("Return node missing retval input".into())
    })?;
    let producer = fg.graph.get_node_from_output(retval);
    match fg.graph.node_kind(producer) {
        NodeKind::IntConst(v) => assert_eq!(*v, 3, "popcount(0b1011) == 3"),
        other => panic!("expected IntConst after rewrite, got {other:?}"),
    }
    Ok(())
}

/// Multi-capture rule exercising `int_binary_any` + `int_const_with!`
/// with an op-variant capture.  Rewrites `Add(IntConst(1,U32),
/// IntConst(2,U32))` to `IntConst(3, U32)`.
#[test]
fn int_const_with_int_binary_any_add() -> Result<()> {
    use crate::pat::int_binary_any;
    use crate::var::{IntBinaryOpVar, IntVar};

    let mut b = FunctionBuilder::new_raw(vec![], &[], &[], &[], None, 0)?;
    let r = b.create_region()?;
    b.set_entry_region(r)?;
    b.set_region(r);
    let one = b.build_int_const(1, NodeOutputType::U32);
    let two = b.build_int_const(2, NodeOutputType::U32);
    let add_out =
        b.build_int_binary_operation(one, two, IntBinaryOp::Add, NodeOutputType::U32)?;
    b.build_return(Some(add_out), &[])?;
    let mut fg = b.build()?;

    let add_node = fg.graph.get_node_from_output(add_out);

    let op = IntBinaryOpVar::new();
    let l = IntVar::new();
    let rr = IntVar::new();

    // A tiny evaluator: enough for Add/Sub/Mul at test-scope.
    fn eval_simple(op: IntBinaryOp, l: u64, r: u64) -> Option<u64> {
        match op {
            IntBinaryOp::Add => Some(l.wrapping_add(r)),
            IntBinaryOp::Sub => Some(l.wrapping_sub(r)),
            IntBinaryOp::Mul => Some(l.wrapping_mul(r)),
            _ => None,
        }
    }

    // Matches LHS using `any_int_const(IntVar)` to bind each operand's
    // value; both operand orderings considered automatically by the
    // commutative-match path.
    use crate::pat::any_int_const;
    let rule = rewrite_rule(
        int_binary_any(op, any_int_const(l), any_int_const(rr)),
        int_const_with!([op, l, rr, ty] => {
            // `ty` is bound by the macro to the root's output type;
            // read it to exercise the auto-binding path.
            let _ty: NodeOutputType = ty;
            eval_simple(op, l, rr).ok_or_else(|| Error::rewrite_closure(
                std::io::Error::other("unsupported op in test evaluator"),
            ))?
        }),
    );
    let changed = rule(&mut fg, add_node)?;
    assert!(changed, "int_binary_any+Add rule should fire");

    // Locate the Return and verify its retval is IntConst(3).
    let ret_node = fg
        .preorder()
        .find(|&n| matches!(fg.graph.node_kind(n), NodeKind::Return))
        .ok_or_else(|| ErrorKind::AssertionFailed("no Return node".into()))?;
    let ret_inputs: Vec<NodeOutputId> =
        fg.graph.node_inputs(ret_node).into_iter().collect();
    let retval = ret_inputs.get(1).copied().ok_or_else(|| {
        ErrorKind::AssertionFailed("Return node missing retval".into())
    })?;
    let producer = fg.graph.get_node_from_output(retval);
    match fg.graph.node_kind(producer) {
        NodeKind::IntConst(v) => assert_eq!(*v, 3),
        other => panic!("expected IntConst, got {other:?}"),
    }
    Ok(())
}

/// `bool_const_with!`: rewrites `BoolUnary(Neg, BoolConst(true))`
/// to `BoolConst(false)`.  Exercises the `BoolVar` typed capture
/// end-to-end.
#[test]
fn bool_const_with_not_rewrite() -> Result<()> {
    use crate::pat::{any_bool_const, bool_unary};
    use crate::var::BoolVar;
    use ir::BoolUnaryOp;

    let mut b = FunctionBuilder::new_raw(vec![], &[], &[], &[], None, 0)?;
    let r = b.create_region()?;
    b.set_entry_region(r)?;
    b.set_region(r);
    let t = b.build_boolean_const(true);
    let notted = b.build_boolean_unary_operation(t, BoolUnaryOp::Neg)?;
    // Return directly from the Bool output — `build_return` accepts
    // any value type on the ret-val slot.
    b.build_return(Some(notted), &[])?;
    let mut fg = b.build()?;

    let not_node = fg.graph.get_node_from_output(notted);

    let bv = BoolVar::new();
    let rule = rewrite_rule(
        bool_unary(BoolUnaryOp::Neg, any_bool_const(bv)),
        bool_const_with!([bv] => !bv),
    );
    let changed = rule(&mut fg, not_node)?;
    assert!(changed, "bool_const_with Neg rule should fire");

    // The Return input should now be a BoolConst(false).
    let ret_node = fg
        .preorder()
        .find(|&n| matches!(fg.graph.node_kind(n), NodeKind::Return))
        .ok_or_else(|| ErrorKind::AssertionFailed("no Return node".into()))?;
    let ret_inputs: Vec<NodeOutputId> =
        fg.graph.node_inputs(ret_node).into_iter().collect();
    let retval = ret_inputs.get(1).copied().ok_or_else(|| {
        ErrorKind::AssertionFailed("Return node missing retval".into())
    })?;
    let producer = fg.graph.get_node_from_output(retval);
    match fg.graph.node_kind(producer) {
        NodeKind::BoolConst(v) => assert!(!*v, "!true == false"),
        other => panic!("expected BoolConst after rewrite, got {other:?}"),
    }
    Ok(())
}

/// `float_const_with!`: flips the sign bit of a `FloatConst(1.0f64)`,
/// yielding `FloatConst(-1.0f64)`.  Exercises the `FloatVar` typed
/// capture end-to-end.
#[test]
fn float_const_with_signbit_flip() -> Result<()> {
    use crate::pat::any_float_const;
    use crate::var::FloatVar;

    // Minimal graph: return a FloatConst(1.0, F64).  The rule fires on
    // the `FloatConst` root directly.
    let mut b = FunctionBuilder::new_raw(vec![], &[], &[], &[], None, 0)?;
    let r = b.create_region()?;
    b.set_entry_region(r)?;
    b.set_region(r);
    let f_out = b.build_float_const(1.0f64.to_bits(), NodeOutputType::F64);
    b.build_return(Some(f_out), &[])?;
    let mut fg = b.build()?;

    let f_node = fg.graph.get_node_from_output(f_out);

    let f = FloatVar::new();
    let signbit = 0x8000_0000_0000_0000u64;
    let rule = rewrite_rule(
        any_float_const(f),
        float_const_with!([f] => f ^ signbit),
    );
    let changed = rule(&mut fg, f_node)?;
    assert!(changed, "float_const_with sign-flip rule should fire");

    // The Return input should now be FloatConst(-1.0).
    let ret_node = fg
        .preorder()
        .find(|&n| matches!(fg.graph.node_kind(n), NodeKind::Return))
        .ok_or_else(|| ErrorKind::AssertionFailed("no Return node".into()))?;
    let ret_inputs: Vec<NodeOutputId> =
        fg.graph.node_inputs(ret_node).into_iter().collect();
    let retval = ret_inputs.get(1).copied().ok_or_else(|| {
        ErrorKind::AssertionFailed("Return node missing retval".into())
    })?;
    let producer = fg.graph.get_node_from_output(retval);
    match fg.graph.node_kind(producer) {
        NodeKind::FloatConst(bits) => {
            assert_eq!(*bits, (-1.0f64).to_bits(), "sign-bit flip of +1.0 is -1.0");
        }
        other => panic!("expected FloatConst, got {other:?}"),
    }
    Ok(())
}

/// A closure returning `Err(pattern::Error::rewrite_closure(...))`
/// surfaces through the rewrite engine as `Err(_)`, not a panic.
#[test]
fn int_const_with_closure_error_surfaces_via_result() -> Result<()> {
    use crate::pat::{any_int_const, popcount};
    use crate::var::IntVar;

    let mut b = FunctionBuilder::new_raw(vec![], &[], &[], &[], None, 0)?;
    let r = b.create_region()?;
    b.set_entry_region(r)?;
    b.set_region(r);
    let c = b.build_int_const(0, NodeOutputType::U32);
    let pc_out = b.build_popcount(c, NodeOutputType::U32)?;
    b.build_return(Some(pc_out), &[])?;
    let mut fg = b.build()?;
    let pc_node = fg.graph.get_node_from_output(pc_out);

    #[derive(Debug, thiserror::Error)]
    #[error("deliberate test error")]
    struct E;

    let v = IntVar::new();
    let rule = rewrite_rule(
        popcount(any_int_const(v)),
        int_const_with!([v] => {
            // Force an error surface via `?` inside the body;
            // the body's `Result<u64>` context propagates it out.
            let _ = v;
            Err::<u64, _>(Error::rewrite_closure(E))?
        }),
    );
    let err = rule(&mut fg, pc_node).expect_err("rule should surface closure error");
    let msg = format!("{err}");
    assert!(
        msg.contains("deliberate test error") || msg.contains("rewrite-rule closure"),
        "error should mention the closure failure, got: {msg}"
    );
    Ok(())
}

/// A closure that returns `Err(pattern::Error::skip())` must be treated
/// as "rule doesn't apply" by the `rewrite_rule` interpreter, not as a
/// hard error.  The return value is `Ok(false)` and the graph is left
/// untouched.
#[test]
fn int_const_with_skip_returns_ok_false() -> Result<()> {
    use crate::pat::{any_int_const, popcount};
    use crate::var::IntVar;

    let mut b = FunctionBuilder::new_raw(vec![], &[], &[], &[], None, 0)?;
    let r = b.create_region()?;
    b.set_entry_region(r)?;
    b.set_region(r);
    let c = b.build_int_const(0, NodeOutputType::U32);
    let pc_out = b.build_popcount(c, NodeOutputType::U32)?;
    b.build_return(Some(pc_out), &[])?;
    let mut fg = b.build()?;
    let pc_node = fg.graph.get_node_from_output(pc_out);

    let v = IntVar::new();
    let rule = rewrite_rule(
        popcount(any_int_const(v)),
        int_const_with!([v] => {
            let _ = v;
            // Partial oracle decided the rule doesn't apply.
            None::<u64>.ok_or_else(Error::skip)?
        }),
    );
    let changed = rule(&mut fg, pc_node)?;
    assert!(
        !changed,
        "Error::skip() inside a closure should map to Ok(false)"
    );
    // Return should still point at the original popcount node.
    let ret_node = fg
        .preorder()
        .find(|&n| matches!(fg.graph.node_kind(n), NodeKind::Return))
        .ok_or_else(|| ErrorKind::AssertionFailed("no Return node".into()))?;
    let ret_inputs: Vec<NodeOutputId> =
        fg.graph.node_inputs(ret_node).into_iter().collect();
    let retval = ret_inputs.get(1).copied().ok_or_else(|| {
        ErrorKind::AssertionFailed("Return node missing retval".into())
    })?;
    let producer = fg.graph.get_node_from_output(retval);
    assert!(
        matches!(fg.graph.node_kind(producer), NodeKind::Popcount),
        "graph should be untouched after a skip"
    );
    Ok(())
}

/// `Error::skip()` is distinguishable from other error kinds via
/// `is_skip()`, so the `rewrite_rule` interpreter can safely demultiplex
/// them.
#[test]
fn error_skip_is_detectable() {
    let e = Error::skip();
    assert!(e.is_skip(), "Error::skip() should report is_skip() == true");

    let other = Error::from(ErrorKind::AssertionFailed("nope".into()));
    assert!(
        !other.is_skip(),
        "non-skip errors should report is_skip() == false"
    );
}
