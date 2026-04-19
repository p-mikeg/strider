use ir::{
    BoolBinaryOp, BoolUnaryOp, FloatBinaryOp, FloatCmpOp, FloatUnaryOp, FunctionBuilder,
    IntBinaryOp, IntCmpOp, IntUnaryOp,
    node::NodeOutputType,
};
use pattern::*;

use super::common::*;

// ── Variant-agnostic op capture tests (Phase A2) ──────────────────────────────

// ── int_binary_any ────────────────────────────────────────────────────────────

/// Positive match: `int_binary_any` on `Add(5, 3)` captures `IntBinaryOp::Add`.
#[test]
fn int_binary_any_captures_add_variant() -> ir::Result<()> {
    let g = graph_add_return()?;
    let m = Matcher::new(&g);
    let op_var = IntBinaryOpVar::new();
    let hits = m.find_all(&int_binary_any(op_var, any(), any()));
    assert_eq!(hits.len(), 1, "one IntBinaryOp node (Add) in graph");
    assert_eq!(
        hits[0].get_int_binary_op(op_var),
        Some(IntBinaryOp::Add),
        "captured op must be Add"
    );
    Ok(())
}

/// Positive match: `int_binary_any` on `Sub(5, 3)` captures `IntBinaryOp::Sub`.
#[test]
fn int_binary_any_captures_sub_variant() -> ir::Result<()> {
    let g = graph_sub_5_3()?;
    let m = Matcher::new(&g);
    let op_var = IntBinaryOpVar::new();
    let hits = m.find_all(&int_binary_any(op_var, any(), any()));
    assert_eq!(hits.len(), 1, "one IntBinaryOp node (Sub) in graph");
    assert_eq!(
        hits[0].get_int_binary_op(op_var),
        Some(IntBinaryOp::Sub),
        "captured op must be Sub"
    );
    Ok(())
}

/// `int_binary_any` does NOT match an `IntCmpOp` node.
#[test]
fn int_binary_any_does_not_match_cmp_node() -> ir::Result<()> {
    let g = graph_if_branches()?;
    let m = Matcher::new(&g);
    let op_var = IntBinaryOpVar::new();
    // graph_if_branches has IntCmpOp::Equal but no bare IntBinaryOp node in the
    // preorder as a root node matching int_binary_any.
    // find_all will walk all nodes; IntCmpOp is a different NodeKind so no match.
    let hits = m.find_all(&int_binary_any(op_var, int_const(4), int_const(1)));
    assert!(
        hits.is_empty(),
        "int_binary_any must not match IntCmpOp nodes"
    );
    Ok(())
}

/// Commutativity: `int_binary_any(op, int_const(3), int_const(5))` matches
/// `Add(5, 3)` because Add is commutative.
#[test]
fn int_binary_any_commutative_add_reversed() -> ir::Result<()> {
    let g = graph_add_5_3()?;
    let m = Matcher::new(&g);
    let op_var = IntBinaryOpVar::new();
    let hits = m.find_all(&int_binary_any(op_var, int_const(3), int_const(5)));
    assert_eq!(hits.len(), 1, "commutative Add must match reversed operands");
    assert_eq!(hits[0].get_int_binary_op(op_var), Some(IntBinaryOp::Add));
    Ok(())
}

/// Non-commutativity: `int_binary_any(op, int_const(3), int_const(5))` does
/// NOT match `Sub(5, 3)` — Sub is not commutative.
#[test]
fn int_binary_any_non_commutative_sub_wrong_order_no_match() -> ir::Result<()> {
    let g = graph_sub_5_3()?;
    let m = Matcher::new(&g);
    let op_var = IntBinaryOpVar::new();
    // Sub is not commutative: pattern (3, 5) must not match node (5, 3).
    let hits = m.find_all(&int_binary_any(op_var, int_const(3), int_const(5)));
    assert!(
        hits.is_empty(),
        "Sub is not commutative; reversed operands must not match"
    );
    Ok(())
}

// ── int_unary_any ─────────────────────────────────────────────────────────────

/// Positive match: `int_unary_any` on `Neg(add(5,3))` captures `IntUnaryOp::Neg`.
#[test]
fn int_unary_any_captures_neg_variant() -> ir::Result<()> {
    let g = graph_neg_add_return()?;
    let m = Matcher::new(&g);
    let op_var = IntUnaryOpVar::new();
    let hits = m.find_all(&int_unary_any(op_var, any()));
    assert_eq!(hits.len(), 1, "one IntUnaryOp node (Neg) in graph");
    assert_eq!(
        hits[0].get_int_unary_op(op_var),
        Some(IntUnaryOp::Neg),
        "captured op must be Neg"
    );
    Ok(())
}

/// `int_unary_any` does NOT match in a graph with only binary ops.
#[test]
fn int_unary_any_no_match_in_binary_only_graph() -> ir::Result<()> {
    let g = graph_add_return()?;
    let m = Matcher::new(&g);
    let op_var = IntUnaryOpVar::new();
    let hits = m.find_all(&int_unary_any(op_var, any()));
    assert!(
        hits.is_empty(),
        "add-only graph has no IntUnaryOp nodes"
    );
    Ok(())
}

// ── int_cmp_any ───────────────────────────────────────────────────────────────

/// Positive match: `int_cmp_any` on `Equal(4, 1)` captures `IntCmpOp::Equal`.
#[test]
fn int_cmp_any_captures_equal_variant() -> ir::Result<()> {
    let g = graph_if_branches()?;
    let m = Matcher::new(&g);
    let op_var = IntCmpOpVar::new();
    let hits = m.find_all(&int_cmp_any(op_var, any(), any()));
    assert_eq!(hits.len(), 1, "one IntCmpOp node (Equal) in graph");
    assert_eq!(
        hits[0].get_int_cmp_op(op_var),
        Some(IntCmpOp::Equal),
        "captured op must be Equal"
    );
    Ok(())
}

/// `int_cmp_any` (commutative for Equal): reversed operands still match.
#[test]
fn int_cmp_any_equal_commutative_reversed() -> ir::Result<()> {
    let g = graph_if_branches()?;
    let m = Matcher::new(&g);
    let op_var = IntCmpOpVar::new();
    // graph has Equal(4, 1); pattern (1, 4) must still match via commutativity.
    let hits = m.find_all(&int_cmp_any(op_var, int_const(1), int_const(4)));
    assert_eq!(hits.len(), 1, "Equal is commutative; reversed operands must match");
    assert_eq!(hits[0].get_int_cmp_op(op_var), Some(IntCmpOp::Equal));
    Ok(())
}

/// `int_cmp_any` does NOT match in a graph with no comparisons.
#[test]
fn int_cmp_any_no_match_in_add_only_graph() -> ir::Result<()> {
    let g = graph_add_return()?;
    let m = Matcher::new(&g);
    let op_var = IntCmpOpVar::new();
    let hits = m.find_all(&int_cmp_any(op_var, any(), any()));
    assert!(hits.is_empty(), "add-only graph has no IntCmpOp nodes");
    Ok(())
}

// ── bool_binary_any ───────────────────────────────────────────────────────────

/// Positive match: `bool_binary_any` on a `BoolBinaryOp::Or` node captures Or.
///
/// We build `bool_or(true, false)` explicitly. If the optimizer constant-folds
/// it, we won't see a node — so we prevent that by using a non-constant input.
#[test]
fn bool_binary_any_captures_or_variant() -> ir::Result<()> {
    // Build bool_or(cast_to_bool(any_add), true) so the Or cannot be folded.
    let mut b = FunctionBuilder::new(vec![], &[], &[], &[])?;
    let r = b.create_region()?;
    b.set_entry_region(r)?;
    b.set_region(r);
    let c5 = b.build_int_const(5, NodeOutputType::U64);
    let c3 = b.build_int_const(3, NodeOutputType::U64);
    let sum = b.build_int_binary_operation(c5, c3, IntBinaryOp::Add, NodeOutputType::U64)?;
    let casted = b.convert_to_bool_if_needed(sum)?;
    let t = b.build_boolean_const(true);
    let bor = b.build_boolean_operation(casted, t, BoolBinaryOp::Or)?;
    let as_int = b.convert_to_int_if_needed(bor, NodeOutputType::U64)?;
    b.build_return(Some(as_int), &[])?;
    let g = b.build().expect("build failed: validator rejected graph");

    let m = Matcher::new(&g);
    let op_var = BoolBinaryOpVar::new();
    let hits = m.find_all(&bool_binary_any(op_var, any(), any()));
    assert_eq!(hits.len(), 1, "one BoolBinaryOp node (Or) expected");
    assert_eq!(
        hits[0].get_bool_binary_op(op_var),
        Some(BoolBinaryOp::Or),
        "captured op must be Or"
    );
    Ok(())
}

/// `bool_binary_any` does NOT match in a graph with no boolean binary ops.
#[test]
fn bool_binary_any_no_match_in_add_only_graph() -> ir::Result<()> {
    let g = graph_add_return()?;
    let m = Matcher::new(&g);
    let op_var = BoolBinaryOpVar::new();
    let hits = m.find_all(&bool_binary_any(op_var, any(), any()));
    assert!(hits.is_empty(), "add-only graph has no BoolBinaryOp nodes");
    Ok(())
}

// ── bool_unary_any ────────────────────────────────────────────────────────────

/// Positive match: `bool_unary_any` on `BoolUnaryOp::Neg(true)` captures Neg.
#[test]
fn bool_unary_any_captures_neg_variant() -> ir::Result<()> {
    let g = graph_bool_not_return()?;
    let m = Matcher::new(&g);
    let op_var = BoolUnaryOpVar::new();
    let hits = m.find_all(&bool_unary_any(op_var, any()));
    assert_eq!(hits.len(), 1, "one BoolUnaryOp node (Neg) expected");
    assert_eq!(
        hits[0].get_bool_unary_op(op_var),
        Some(BoolUnaryOp::Neg),
        "captured op must be Neg"
    );
    Ok(())
}

/// `bool_unary_any` does NOT match in a graph with no boolean unary ops.
#[test]
fn bool_unary_any_no_match_in_add_only_graph() -> ir::Result<()> {
    let g = graph_add_return()?;
    let m = Matcher::new(&g);
    let op_var = BoolUnaryOpVar::new();
    let hits = m.find_all(&bool_unary_any(op_var, any()));
    assert!(hits.is_empty(), "add-only graph has no BoolUnaryOp nodes");
    Ok(())
}

// ── float_binary_any ──────────────────────────────────────────────────────────

/// Positive match: `float_binary_any` on `FloatAdd(1.0, 2.0)` captures Add.
#[test]
fn float_binary_any_captures_add_variant() -> ir::Result<()> {
    let mut b = FunctionBuilder::new(vec![], &[], &[], &[])?;
    let r = b.create_region()?;
    b.set_entry_region(r)?;
    b.set_region(r);
    let c1 = b.build_float_const(1.0f64.to_bits(), NodeOutputType::F64);
    let c2 = b.build_float_const(2.0f64.to_bits(), NodeOutputType::F64);
    let sum = b.build_float_binary_op(c1, c2, FloatBinaryOp::Add, NodeOutputType::F64)?;
    b.build_return(Some(sum), &[])?;
    let g = b.build().expect("build failed: validator rejected graph");

    let m = Matcher::new(&g);
    let op_var = FloatBinaryOpVar::new();
    let hits = m.find_all(&float_binary_any(op_var, any(), any()));
    assert_eq!(hits.len(), 1, "one FloatBinaryOp node (Add) expected");
    assert_eq!(
        hits[0].get_float_binary_op(op_var),
        Some(FloatBinaryOp::Add),
        "captured op must be FloatBinaryOp::Add"
    );
    Ok(())
}

/// Positive match: `float_binary_any` on `FloatDiv` captures Div.
#[test]
fn float_binary_any_captures_div_variant() -> ir::Result<()> {
    let mut b = FunctionBuilder::new(vec![], &[], &[], &[])?;
    let r = b.create_region()?;
    b.set_entry_region(r)?;
    b.set_region(r);
    let c6 = b.build_float_const(6.0f64.to_bits(), NodeOutputType::F64);
    let c3 = b.build_float_const(3.0f64.to_bits(), NodeOutputType::F64);
    let div = b.build_float_binary_op(c6, c3, FloatBinaryOp::Div, NodeOutputType::F64)?;
    b.build_return(Some(div), &[])?;
    let g = b.build().expect("build failed: validator rejected graph");

    let m = Matcher::new(&g);
    let op_var = FloatBinaryOpVar::new();
    let hits = m.find_all(&float_binary_any(op_var, any(), any()));
    assert_eq!(hits.len(), 1, "one FloatBinaryOp node (Div) expected");
    assert_eq!(
        hits[0].get_float_binary_op(op_var),
        Some(FloatBinaryOp::Div),
        "captured op must be FloatBinaryOp::Div"
    );
    Ok(())
}

/// Commutativity: `float_binary_any(op, c2, c1)` matches `FloatAdd(c1, c2)`.
#[test]
fn float_binary_any_commutative_add_reversed() -> ir::Result<()> {
    let mut b = FunctionBuilder::new(vec![], &[], &[], &[])?;
    let r = b.create_region()?;
    b.set_entry_region(r)?;
    b.set_region(r);
    let c1 = b.build_float_const(1.0f64.to_bits(), NodeOutputType::F64);
    let c2 = b.build_float_const(2.0f64.to_bits(), NodeOutputType::F64);
    let sum = b.build_float_binary_op(c1, c2, FloatBinaryOp::Add, NodeOutputType::F64)?;
    b.build_return(Some(sum), &[])?;
    let g = b.build().expect("build failed: validator rejected graph");

    let m = Matcher::new(&g);
    let op_var = FloatBinaryOpVar::new();
    // Reversed order: pattern (2.0, 1.0) vs node (1.0, 2.0) — Add is commutative.
    let hits = m.find_all(&float_binary_any(
        op_var,
        float_const(2.0f64.to_bits()),
        float_const(1.0f64.to_bits()),
    ));
    assert_eq!(hits.len(), 1, "FloatAdd is commutative; reversed operands match");
    assert_eq!(hits[0].get_float_binary_op(op_var), Some(FloatBinaryOp::Add));
    Ok(())
}

// ── float_unary_any ───────────────────────────────────────────────────────────

/// Positive match: `float_unary_any` on `FloatNeg(2.0)` captures Neg.
#[test]
fn float_unary_any_captures_neg_variant() -> ir::Result<()> {
    let mut b = FunctionBuilder::new(vec![], &[], &[], &[])?;
    let r = b.create_region()?;
    b.set_entry_region(r)?;
    b.set_region(r);
    let cv = b.build_float_const(2.0f64.to_bits(), NodeOutputType::F64);
    let neg_v = b.build_float_unary_op(cv, FloatUnaryOp::Neg, NodeOutputType::F64)?;
    b.build_return(Some(neg_v), &[])?;
    let g = b.build().expect("build failed: validator rejected graph");

    let m = Matcher::new(&g);
    let op_var = FloatUnaryOpVar::new();
    let hits = m.find_all(&float_unary_any(op_var, any()));
    assert_eq!(hits.len(), 1, "one FloatUnaryOp node (Neg) expected");
    assert_eq!(
        hits[0].get_float_unary_op(op_var),
        Some(FloatUnaryOp::Neg),
        "captured op must be FloatUnaryOp::Neg"
    );
    Ok(())
}

/// `float_unary_any` does NOT match in a graph with only integer ops.
#[test]
fn float_unary_any_no_match_in_int_only_graph() -> ir::Result<()> {
    let g = graph_add_return()?;
    let m = Matcher::new(&g);
    let op_var = FloatUnaryOpVar::new();
    let hits = m.find_all(&float_unary_any(op_var, any()));
    assert!(hits.is_empty(), "int-only graph has no FloatUnaryOp nodes");
    Ok(())
}

// ── float_cmp_any ─────────────────────────────────────────────────────────────

/// Positive match: `float_cmp_any` on `FloatCmp::Less(3.0, 4.0)` captures Less.
#[test]
fn float_cmp_any_captures_less_variant() -> ir::Result<()> {
    let mut b = FunctionBuilder::new(vec![], &[], &[], &[])?;
    let r = b.create_region()?;
    b.set_entry_region(r)?;
    b.set_region(r);
    let c3 = b.build_float_const(3.0f64.to_bits(), NodeOutputType::F64);
    let c4 = b.build_float_const(4.0f64.to_bits(), NodeOutputType::F64);
    let cmp = b.build_float_cmp_op(c3, c4, FloatCmpOp::Less)?;
    b.build_return(Some(cmp), &[])?;
    let g = b.build().expect("build failed: validator rejected graph");

    let m = Matcher::new(&g);
    let op_var = FloatCmpOpVar::new();
    let hits = m.find_all(&float_cmp_any(op_var, any(), any()));
    assert_eq!(hits.len(), 1, "one FloatCmpOp node (Less) expected");
    assert_eq!(
        hits[0].get_float_cmp_op(op_var),
        Some(FloatCmpOp::Less),
        "captured op must be FloatCmpOp::Less"
    );
    Ok(())
}

/// `float_cmp_any` captures `Equal` vs `Less` correctly across two graphs.
#[test]
fn float_cmp_any_equal_vs_less_distinguished() -> ir::Result<()> {
    // Graph with FloatCmp::Equal.
    let mut b = FunctionBuilder::new(vec![], &[], &[], &[])?;
    let r = b.create_region()?;
    b.set_entry_region(r)?;
    b.set_region(r);
    let c1 = b.build_float_const(1.0f64.to_bits(), NodeOutputType::F64);
    let c2 = b.build_float_const(2.0f64.to_bits(), NodeOutputType::F64);
    let cmp_eq = b.build_float_cmp_op(c1, c2, FloatCmpOp::Equal)?;
    b.build_return(Some(cmp_eq), &[])?;
    let g_eq = b.build().expect("build failed: validator rejected graph");

    let op_var = FloatCmpOpVar::new();
    let m_eq = Matcher::new(&g_eq);
    let hits_eq = m_eq.find_all(&float_cmp_any(op_var, any(), any()));
    assert_eq!(hits_eq.len(), 1);
    assert_eq!(hits_eq[0].get_float_cmp_op(op_var), Some(FloatCmpOp::Equal));

    // Graph with FloatCmp::Less.
    let mut b2 = FunctionBuilder::new(vec![], &[], &[], &[])?;
    let r2 = b2.create_region()?;
    b2.set_entry_region(r2)?;
    b2.set_region(r2);
    let d1 = b2.build_float_const(1.0f64.to_bits(), NodeOutputType::F64);
    let d2 = b2.build_float_const(2.0f64.to_bits(), NodeOutputType::F64);
    let cmp_lt = b2.build_float_cmp_op(d1, d2, FloatCmpOp::Less)?;
    b2.build_return(Some(cmp_lt), &[])?;
    let g_lt = b2.build().expect("build failed: validator rejected graph");

    let op_var2 = FloatCmpOpVar::new();
    let m_lt = Matcher::new(&g_lt);
    let hits_lt = m_lt.find_all(&float_cmp_any(op_var2, any(), any()));
    assert_eq!(hits_lt.len(), 1);
    assert_eq!(hits_lt[0].get_float_cmp_op(op_var2), Some(FloatCmpOp::Less));
    Ok(())
}
