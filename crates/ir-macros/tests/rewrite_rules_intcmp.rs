//! Tests for `rewrite_rules!` grammar extensions covering integer comparison
//! operations: `IntEq`, `IntLt`, `IntLe`, `IntSlt`, `IntSle`, `IntCarry`,
//! `IntBorrow`, `IntScarry`, `IntSborrow` (all map to `NodeKind::IntCmpOp`).
//!
//! Also tests that `bool_const(expr)` on the RHS accepts arbitrary Rust
//! expressions (not just boolean literals).

#![allow(
    clippy::panic,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::unreachable
)]

use ir::node::{NodeKind, NodeOutputType};
use ir::{FunctionBuilder, IntCmpOp};
use ir_macros::rewrite_rules;
use opt::OptimizationResult;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// Build a minimal `BuiltFunctionGraph` with no real body — we only need a
/// valid graph to attach orphan nodes.
fn empty_fg() -> ir::BuiltFunctionGraph {
    let mut b = FunctionBuilder::new(vec![], &[], &[], &[]).unwrap();
    let region = b.create_region().unwrap();
    b.set_entry_region(region).unwrap();
    b.set_region(region);
    let v = b.build_int_const(0, NodeOutputType::U32);
    b.build_return(Some(v), &[]).unwrap();
    b.build().unwrap()
}

// ── IntEq: bool_const(l == r) RHS with captured ints ────────────────────────

/// Rule `IntEq(IntConst(l), IntConst(r)) => bool_const(l == r)`.
/// Both operands are the same constant value (5), so the equality holds and
/// `bool_const(true)` should be produced, yielding `Changed`.
///
/// This test also validates that `bool_const(expr)` on the RHS accepts an
/// arbitrary Rust expression (`l == r`), not just a boolean literal.
#[test]
fn int_eq_constant_equality() -> Result<()> {
    let mut fg = empty_fg();

    let c1 = fg.make_int_const(5, NodeOutputType::U32)?;
    let c2 = fg.make_int_const(5, NodeOutputType::U32)?;
    let eq_out = fg.make_value_node(
        NodeKind::IntCmpOp(IntCmpOp::Equal),
        [c1, c2],
        NodeOutputType::Bool,
    )?;

    // Give eq_out a user so replace_all_uses has something to redirect.
    let sink = fg.make_bool_const(false)?;
    fg.make_value_node(
        NodeKind::BoolBinaryOp(ir::BoolBinaryOp::Or),
        [eq_out, sink],
        NodeOutputType::Bool,
    )?;

    let apply = rewrite_rules! {
        IntEq(IntConst(l), IntConst(r)) => bool_const(l == r),
    };

    let eq_node = fg.graph.get_node_from_output(eq_out);
    let res = apply(&mut fg, eq_node)?;
    assert_eq!(res, OptimizationResult::Changed);

    // eq_out should now have no users.
    assert!(
        fg.graph.output_use_cursor(eq_out).current().is_none(),
        "IntEq output should have no users after the rewrite"
    );

    Ok(())
}

// ── IntEq commutative: both orderings match ───────────────────────────────────

/// IntEq is commutative. A rule that places `IntConst(5)` on the LEFT should
/// still fire when the constant is on the RIGHT.
#[test]
fn int_eq_commutative_both_orderings() -> Result<()> {
    let mut fg = empty_fg();

    let five = fg.make_int_const(5, NodeOutputType::U32)?;
    let ten = fg.make_int_const(10, NodeOutputType::U32)?;

    // Build IntEq(ten, five) — the constant `5` is on the RIGHT.
    let eq_out = fg.make_value_node(
        NodeKind::IntCmpOp(IntCmpOp::Equal),
        [ten, five],
        NodeOutputType::Bool,
    )?;

    let sink = fg.make_bool_const(true)?;
    fg.make_value_node(
        NodeKind::BoolBinaryOp(ir::BoolBinaryOp::And),
        [eq_out, sink],
        NodeOutputType::Bool,
    )?;

    // Rule places 5 on the LEFT — commutative matching should still fire.
    let apply = rewrite_rules! {
        IntEq(IntConst(5), x) => bool_const(false),
    };

    let eq_node = fg.graph.get_node_from_output(eq_out);
    let res = apply(&mut fg, eq_node)?;
    assert_eq!(res, OptimizationResult::Changed);

    Ok(())
}

// ── IntLt: NOT commutative ────────────────────────────────────────────────────

/// `IntLt` is NOT commutative. A rule requiring `IntConst(0)` on the LEFT should
/// NOT fire when zero is on the RIGHT.
#[test]
fn int_lt_not_commutative_no_match() -> Result<()> {
    let mut fg = empty_fg();

    let one = fg.make_int_const(1, NodeOutputType::U32)?;
    let zero = fg.make_int_const(0, NodeOutputType::U32)?;
    // IntLt(one, zero) — zero is on the RIGHT.
    let lt_out = fg.make_value_node(
        NodeKind::IntCmpOp(IntCmpOp::Less),
        [one, zero],
        NodeOutputType::Bool,
    )?;

    let sink = fg.make_bool_const(false)?;
    fg.make_value_node(
        NodeKind::BoolBinaryOp(ir::BoolBinaryOp::Or),
        [lt_out, sink],
        NodeOutputType::Bool,
    )?;

    // Rule: IntLt(IntConst(0), x) — zero must be on the LEFT.
    let apply = rewrite_rules! {
        IntLt(IntConst(0), x) => bool_const(true),
    };

    let lt_node = fg.graph.get_node_from_output(lt_out);
    let res = apply(&mut fg, lt_node)?;
    // Must NOT fire — operands are in wrong order and IntLt is non-commutative.
    assert_eq!(res, OptimizationResult::NoChange);

    Ok(())
}

/// `IntLt(IntConst(0), x)` DOES fire when zero is on the LEFT.
#[test]
fn int_lt_fires_when_zero_on_left() -> Result<()> {
    let mut fg = empty_fg();

    let zero = fg.make_int_const(0, NodeOutputType::U32)?;
    let one = fg.make_int_const(1, NodeOutputType::U32)?;
    // IntLt(zero, one) — zero is on the LEFT.
    let lt_out = fg.make_value_node(
        NodeKind::IntCmpOp(IntCmpOp::Less),
        [zero, one],
        NodeOutputType::Bool,
    )?;

    let sink = fg.make_bool_const(false)?;
    fg.make_value_node(
        NodeKind::BoolBinaryOp(ir::BoolBinaryOp::Or),
        [lt_out, sink],
        NodeOutputType::Bool,
    )?;

    let apply = rewrite_rules! {
        IntLt(IntConst(0), x) => bool_const(true),
    };

    let lt_node = fg.graph.get_node_from_output(lt_out);
    let res = apply(&mut fg, lt_node)?;
    assert_eq!(res, OptimizationResult::Changed);

    Ok(())
}

// ── IntCarry: commutative ────────────────────────────────────────────────────

/// `IntCarry` is commutative (addition carry). A rule that places the constant
/// on the LEFT should also fire when it is on the RIGHT.
#[test]
fn int_carry_commutative() -> Result<()> {
    let mut fg = empty_fg();

    let c = fg.make_int_const(42, NodeOutputType::U32)?;
    let x = fg.make_int_const(99, NodeOutputType::U32)?;
    // IntCarry(x, c) — the constant 42 is on the RIGHT.
    let carry_out = fg.make_value_node(
        NodeKind::IntCmpOp(IntCmpOp::Carry),
        [x, c],
        NodeOutputType::Bool,
    )?;

    let sink = fg.make_bool_const(false)?;
    fg.make_value_node(
        NodeKind::BoolBinaryOp(ir::BoolBinaryOp::And),
        [carry_out, sink],
        NodeOutputType::Bool,
    )?;

    // Rule: IntCarry(IntConst(42), y) — constant is on LEFT.
    // Commutative matching should flip and still match.
    let apply = rewrite_rules! {
        IntCarry(IntConst(42), y) => bool_const(false),
    };

    let carry_node = fg.graph.get_node_from_output(carry_out);
    let res = apply(&mut fg, carry_node)?;
    assert_eq!(res, OptimizationResult::Changed);

    Ok(())
}

// ── IntBorrow: NOT commutative ───────────────────────────────────────────────

/// `IntBorrow` is NOT commutative (subtraction borrow). The rule should NOT
/// fire when operands are in the wrong order.
#[test]
fn int_borrow_not_commutative() -> Result<()> {
    let mut fg = empty_fg();

    let one = fg.make_int_const(1, NodeOutputType::U32)?;
    let zero = fg.make_int_const(0, NodeOutputType::U32)?;
    // IntBorrow(one, zero) — zero is on the RIGHT, rule needs it on LEFT.
    let borrow_out = fg.make_value_node(
        NodeKind::IntCmpOp(IntCmpOp::Borrow),
        [one, zero],
        NodeOutputType::Bool,
    )?;

    let sink = fg.make_bool_const(true)?;
    fg.make_value_node(
        NodeKind::BoolBinaryOp(ir::BoolBinaryOp::Or),
        [borrow_out, sink],
        NodeOutputType::Bool,
    )?;

    // Rule: IntBorrow(IntConst(0), x) — zero must be on LEFT.
    let apply = rewrite_rules! {
        IntBorrow(IntConst(0), x) => bool_const(false),
    };

    let borrow_node = fg.graph.get_node_from_output(borrow_out);
    let res = apply(&mut fg, borrow_node)?;
    assert_eq!(res, OptimizationResult::NoChange);

    Ok(())
}

// ── IntSlt / IntSle / IntScarry / IntSborrow smoke ───────────────────────────

/// Smoke test `IntSlt` (signed less-than, non-commutative): fires when operands
/// are in the correct stated order.
#[test]
fn int_slt_smoke() -> Result<()> {
    let mut fg = empty_fg();

    let zero = fg.make_int_const(0, NodeOutputType::U32)?;
    let one = fg.make_int_const(1, NodeOutputType::U32)?;
    let slt_out = fg.make_value_node(
        NodeKind::IntCmpOp(IntCmpOp::Sless),
        [zero, one],
        NodeOutputType::Bool,
    )?;

    let sink = fg.make_bool_const(false)?;
    fg.make_value_node(
        NodeKind::BoolBinaryOp(ir::BoolBinaryOp::Or),
        [slt_out, sink],
        NodeOutputType::Bool,
    )?;

    let apply = rewrite_rules! {
        IntSlt(IntConst(0), x) => bool_const(true),
    };

    let slt_node = fg.graph.get_node_from_output(slt_out);
    let res = apply(&mut fg, slt_node)?;
    assert_eq!(res, OptimizationResult::Changed);

    Ok(())
}

/// Smoke test `IntSle` (signed less-than-or-equal, non-commutative).
#[test]
fn int_sle_smoke() -> Result<()> {
    let mut fg = empty_fg();

    let zero = fg.make_int_const(0, NodeOutputType::U32)?;
    let one = fg.make_int_const(1, NodeOutputType::U32)?;
    let sle_out = fg.make_value_node(
        NodeKind::IntCmpOp(IntCmpOp::SlessEqual),
        [zero, one],
        NodeOutputType::Bool,
    )?;

    let sink = fg.make_bool_const(false)?;
    fg.make_value_node(
        NodeKind::BoolBinaryOp(ir::BoolBinaryOp::Or),
        [sle_out, sink],
        NodeOutputType::Bool,
    )?;

    let apply = rewrite_rules! {
        IntSle(IntConst(0), x) => bool_const(true),
    };

    let sle_node = fg.graph.get_node_from_output(sle_out);
    let res = apply(&mut fg, sle_node)?;
    assert_eq!(res, OptimizationResult::Changed);

    Ok(())
}

/// Smoke test `IntLe` (unsigned less-than-or-equal, non-commutative).
#[test]
fn int_le_smoke() -> Result<()> {
    let mut fg = empty_fg();

    let zero = fg.make_int_const(0, NodeOutputType::U32)?;
    let one = fg.make_int_const(1, NodeOutputType::U32)?;
    let le_out = fg.make_value_node(
        NodeKind::IntCmpOp(IntCmpOp::LessEqual),
        [zero, one],
        NodeOutputType::Bool,
    )?;

    let sink = fg.make_bool_const(false)?;
    fg.make_value_node(
        NodeKind::BoolBinaryOp(ir::BoolBinaryOp::Or),
        [le_out, sink],
        NodeOutputType::Bool,
    )?;

    let apply = rewrite_rules! {
        IntLe(IntConst(0), x) => bool_const(true),
    };

    let le_node = fg.graph.get_node_from_output(le_out);
    let res = apply(&mut fg, le_node)?;
    assert_eq!(res, OptimizationResult::Changed);

    Ok(())
}

/// Smoke test `IntScarry` (signed carry, commutative).
#[test]
fn int_scarry_commutative_smoke() -> Result<()> {
    let mut fg = empty_fg();

    let c = fg.make_int_const(7, NodeOutputType::U32)?;
    let x = fg.make_int_const(3, NodeOutputType::U32)?;
    // IntScarry(x, c) — constant 7 is on the RIGHT.
    let scarry_out = fg.make_value_node(
        NodeKind::IntCmpOp(IntCmpOp::Scarry),
        [x, c],
        NodeOutputType::Bool,
    )?;

    let sink = fg.make_bool_const(true)?;
    fg.make_value_node(
        NodeKind::BoolBinaryOp(ir::BoolBinaryOp::And),
        [scarry_out, sink],
        NodeOutputType::Bool,
    )?;

    // Rule: IntScarry(IntConst(7), y) — constant on LEFT; commutative flip needed.
    let apply = rewrite_rules! {
        IntScarry(IntConst(7), y) => bool_const(false),
    };

    let scarry_node = fg.graph.get_node_from_output(scarry_out);
    let res = apply(&mut fg, scarry_node)?;
    assert_eq!(res, OptimizationResult::Changed);

    Ok(())
}

/// Smoke test `IntSborrow` (signed borrow, non-commutative): does NOT fire
/// when operands are in wrong order.
#[test]
fn int_sborrow_not_commutative() -> Result<()> {
    let mut fg = empty_fg();

    let one = fg.make_int_const(1, NodeOutputType::U32)?;
    let zero = fg.make_int_const(0, NodeOutputType::U32)?;
    // IntSborrow(one, zero) — zero is on RIGHT, rule needs it on LEFT.
    let sborrow_out = fg.make_value_node(
        NodeKind::IntCmpOp(IntCmpOp::Sborrow),
        [one, zero],
        NodeOutputType::Bool,
    )?;

    let sink = fg.make_bool_const(true)?;
    fg.make_value_node(
        NodeKind::BoolBinaryOp(ir::BoolBinaryOp::Or),
        [sborrow_out, sink],
        NodeOutputType::Bool,
    )?;

    let apply = rewrite_rules! {
        IntSborrow(IntConst(0), x) => bool_const(false),
    };

    let sborrow_node = fg.graph.get_node_from_output(sborrow_out);
    let res = apply(&mut fg, sborrow_node)?;
    assert_eq!(res, OptimizationResult::NoChange);

    Ok(())
}

// ── bool_const(expr) with expression (not just literal) ─────────────────────

/// Prove that `bool_const(l == r)` on the RHS correctly evaluates the
/// expression when the two captured integers differ — it should produce
/// `bool_const(false)` which is a different value from the replaced node,
/// giving `Changed`.
#[test]
fn bool_const_expr_with_unequal_consts() -> Result<()> {
    let mut fg = empty_fg();

    let c1 = fg.make_int_const(3, NodeOutputType::U32)?;
    let c2 = fg.make_int_const(7, NodeOutputType::U32)?;
    let eq_out = fg.make_value_node(
        NodeKind::IntCmpOp(IntCmpOp::Equal),
        [c1, c2],
        NodeOutputType::Bool,
    )?;

    let sink = fg.make_bool_const(true)?;
    fg.make_value_node(
        NodeKind::BoolBinaryOp(ir::BoolBinaryOp::And),
        [eq_out, sink],
        NodeOutputType::Bool,
    )?;

    // The RHS uses a Rust expression: `l == r` evaluates to `false` here.
    let apply = rewrite_rules! {
        IntEq(IntConst(l), IntConst(r)) => bool_const(l == r),
    };

    let eq_node = fg.graph.get_node_from_output(eq_out);
    let res = apply(&mut fg, eq_node)?;
    assert_eq!(res, OptimizationResult::Changed);

    Ok(())
}
