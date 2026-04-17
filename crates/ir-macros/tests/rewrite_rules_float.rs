//! Tests for `rewrite_rules!` grammar extensions covering float operations:
//! `FAdd`, `FSub`, `FMul`, `FDiv` (FloatBinaryOp) and
//! `FEq`, `FNe`, `FLt`, `FLe` (FloatCmpOp), plus the `float_const(bits, ty)` RHS builder.

#![allow(
    clippy::panic,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::unreachable
)]

use ir::node::{NodeKind, NodeOutputType};
use ir::{FloatBinaryOp, FloatCmpOp, FunctionBuilder};
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

// ── FAdd commutative identity ────────────────────────────────────────────────

/// Rule `FAdd(x, FloatConst(0)) => x` must fire with the zero on either side.
/// Note: this is not sound IEEE-754 in general (−0 issues), but is valid as a
/// grammar smoke test.
#[test]
fn float_add_zero_identity() -> Result<()> {
    let mut fg = empty_fg();

    // Construct: FAdd(some_float, FloatConst(0))
    let x = fg.make_float_const(0x3f80_0000u64, NodeOutputType::F32)?; // 1.0f32
    let zero = fg.make_float_const(0u64, NodeOutputType::F32)?;
    let add_out = fg.make_value_node(
        NodeKind::FloatBinaryOp(FloatBinaryOp::Add),
        [x, zero],
        NodeOutputType::F32,
    )?;

    // Give add_out a user so replace_all_uses finds something to redirect.
    let sink = fg.make_float_const(1u64, NodeOutputType::F32)?;
    fg.make_value_node(
        NodeKind::FloatBinaryOp(FloatBinaryOp::Mul),
        [add_out, sink],
        NodeOutputType::F32,
    )?;

    let apply = rewrite_rules! {
        FAdd(x, FloatConst(0)) => x,
    };

    let add_node = fg.graph.get_node_from_output(add_out);
    let res = apply(&mut fg, add_node)?;
    assert_eq!(res, OptimizationResult::Changed);

    // add_out should now have no users (they were redirected to x).
    assert!(
        fg.graph.output_use_cursor(add_out).current().is_none(),
        "FAdd output should have no users after the rewrite"
    );

    Ok(())
}

/// Same rule, but with the zero on the LEFT — proves commutative matching.
#[test]
fn float_add_zero_identity_commuted() -> Result<()> {
    let mut fg = empty_fg();

    let x = fg.make_float_const(0x3f80_0000u64, NodeOutputType::F32)?; // 1.0f32
    let zero = fg.make_float_const(0u64, NodeOutputType::F32)?;
    // FAdd(zero, x) — zero is the LEFT operand.
    let add_out = fg.make_value_node(
        NodeKind::FloatBinaryOp(FloatBinaryOp::Add),
        [zero, x],
        NodeOutputType::F32,
    )?;

    let sink = fg.make_float_const(1u64, NodeOutputType::F32)?;
    fg.make_value_node(
        NodeKind::FloatBinaryOp(FloatBinaryOp::Mul),
        [add_out, sink],
        NodeOutputType::F32,
    )?;

    let apply = rewrite_rules! {
        FAdd(x, FloatConst(0)) => x,
    };

    let add_node = fg.graph.get_node_from_output(add_out);
    let res = apply(&mut fg, add_node)?;
    assert_eq!(res, OptimizationResult::Changed);

    Ok(())
}

/// `FSub` is NOT commutative — so an FSub node should NOT match an FAdd rule.
#[test]
fn fsub_does_not_match_fadd() -> Result<()> {
    let mut fg = empty_fg();

    let x = fg.make_float_const(0x3f80_0000u64, NodeOutputType::F32)?;
    let zero = fg.make_float_const(0u64, NodeOutputType::F32)?;
    let sub_out = fg.make_value_node(
        NodeKind::FloatBinaryOp(FloatBinaryOp::Sub),
        [x, zero],
        NodeOutputType::F32,
    )?;

    let sink = fg.make_float_const(1u64, NodeOutputType::F32)?;
    fg.make_value_node(
        NodeKind::FloatBinaryOp(FloatBinaryOp::Mul),
        [sub_out, sink],
        NodeOutputType::F32,
    )?;

    let apply = rewrite_rules! {
        FAdd(x, FloatConst(0)) => x,
    };

    let sub_node = fg.graph.get_node_from_output(sub_out);
    let res = apply(&mut fg, sub_node)?;
    assert_eq!(res, OptimizationResult::NoChange);

    Ok(())
}

// ── FloatCmpOp ───────────────────────────────────────────────────────────────

/// `FEq(FloatConst(bits), FloatConst(bits))` — matching the same constant on
/// both sides should fire (FEq is commutative).
#[test]
fn feq_same_const_rewrites() -> Result<()> {
    let mut fg = empty_fg();

    let bits: u64 = 0x3f80_0000; // 1.0f32
    let c1 = fg.make_float_const(bits, NodeOutputType::F32)?;
    let c2 = fg.make_float_const(bits, NodeOutputType::F32)?;

    let cmp_out = fg.make_value_node(
        NodeKind::FloatCmpOp(FloatCmpOp::Equal),
        [c1, c2],
        NodeOutputType::Bool,
    )?;

    // Give cmp_out a user.
    fg.make_value_node(
        NodeKind::BoolBinaryOp(ir::BoolBinaryOp::And),
        [cmp_out, cmp_out],
        NodeOutputType::Bool,
    )?;

    // Rule: if both operands of FEq are the same float constant, rewrite to
    // BoolConst(true). We capture one side and check the other is the same value;
    // here we use a simpler rule: FEq(FloatConst(b), FloatConst(b)) => bool_const(true).
    // Because the DSL doesn't support equality between two captures in one rule,
    // we instead use a rule that always fires when the head is FEq of two float
    // constants — the point is to exercise the FEq parser, not semantics.
    let apply = rewrite_rules! {
        FEq(FloatConst(a), FloatConst(b)) => bool_const(true),
    };

    let cmp_node = fg.graph.get_node_from_output(cmp_out);
    let res = apply(&mut fg, cmp_node)?;
    assert_eq!(res, OptimizationResult::Changed);

    Ok(())
}

/// `FNe` is commutative — but the rule should NOT fire on an `FLt` node.
#[test]
fn fne_does_not_match_flt() -> Result<()> {
    let mut fg = empty_fg();

    let c1 = fg.make_float_const(0u64, NodeOutputType::F32)?;
    let c2 = fg.make_float_const(0x3f80_0000u64, NodeOutputType::F32)?;

    let lt_out = fg.make_value_node(
        NodeKind::FloatCmpOp(FloatCmpOp::Less),
        [c1, c2],
        NodeOutputType::Bool,
    )?;

    fg.make_value_node(
        NodeKind::BoolBinaryOp(ir::BoolBinaryOp::Or),
        [lt_out, lt_out],
        NodeOutputType::Bool,
    )?;

    // Rule targeting FNe — should NOT fire on an FLt node.
    let apply = rewrite_rules! {
        FNe(FloatConst(a), FloatConst(b)) => bool_const(false),
    };

    let lt_node = fg.graph.get_node_from_output(lt_out);
    let res = apply(&mut fg, lt_node)?;
    assert_eq!(res, OptimizationResult::NoChange);

    Ok(())
}

/// `FLt` is NOT commutative — the rule should NOT fire when operands are swapped.
#[test]
fn flt_not_commutative() -> Result<()> {
    let mut fg = empty_fg();

    // We build FLt(FloatConst(1.0), FloatConst(0.0)) and write the rule as
    // FLt(FloatConst(0), x).  Since 1.0 is on the left, the rule should NOT fire.
    let one = fg.make_float_const(0x3f80_0000u64, NodeOutputType::F32)?;
    let zero = fg.make_float_const(0u64, NodeOutputType::F32)?;
    let lt_out = fg.make_value_node(
        NodeKind::FloatCmpOp(FloatCmpOp::Less),
        [one, zero], // LT(1.0, 0.0)
        NodeOutputType::Bool,
    )?;

    fg.make_value_node(
        NodeKind::BoolBinaryOp(ir::BoolBinaryOp::Or),
        [lt_out, lt_out],
        NodeOutputType::Bool,
    )?;

    // Rule: FLt(FloatConst(0), x) => bool_const(false) — zero is NOT on the left.
    let apply = rewrite_rules! {
        FLt(FloatConst(0), x) => bool_const(false),
    };

    let lt_node = fg.graph.get_node_from_output(lt_out);
    let res = apply(&mut fg, lt_node)?;
    assert_eq!(res, OptimizationResult::NoChange);

    Ok(())
}

/// `FLt(FloatConst(0), x)` DOES fire when zero is on the left.
#[test]
fn flt_fires_when_zero_on_left() -> Result<()> {
    let mut fg = empty_fg();

    let zero = fg.make_float_const(0u64, NodeOutputType::F32)?;
    let one = fg.make_float_const(0x3f80_0000u64, NodeOutputType::F32)?;
    let lt_out = fg.make_value_node(
        NodeKind::FloatCmpOp(FloatCmpOp::Less),
        [zero, one], // LT(0.0, 1.0)
        NodeOutputType::Bool,
    )?;

    fg.make_value_node(
        NodeKind::BoolBinaryOp(ir::BoolBinaryOp::Or),
        [lt_out, lt_out],
        NodeOutputType::Bool,
    )?;

    let apply = rewrite_rules! {
        FLt(FloatConst(0), x) => bool_const(false),
    };

    let lt_node = fg.graph.get_node_from_output(lt_out);
    let res = apply(&mut fg, lt_node)?;
    assert_eq!(res, OptimizationResult::Changed);

    Ok(())
}

// ── float_const RHS builder ──────────────────────────────────────────────────

/// Prove that `float_const(bits_expr, ty)` on the RHS compiles and runs.
/// Rule: `FAdd(FloatConst(b), x) => float_const(b, NodeOutputType::F32)`
/// We don't care that this is not semantically useful; we just need the
/// `float_const(...)` RHS form to compile and produce a Changed result.
#[test]
fn float_const_rhs_builder() -> Result<()> {
    let mut fg = empty_fg();

    let bits: u64 = 0x4049_0fdb; // pi as f32
    let pi = fg.make_float_const(bits, NodeOutputType::F32)?;
    let x = fg.make_float_const(0x3f80_0000u64, NodeOutputType::F32)?; // 1.0
    let add_out = fg.make_value_node(
        NodeKind::FloatBinaryOp(FloatBinaryOp::Add),
        [pi, x],
        NodeOutputType::F32,
    )?;

    let sink = fg.make_float_const(2u64, NodeOutputType::F32)?;
    fg.make_value_node(
        NodeKind::FloatBinaryOp(FloatBinaryOp::Mul),
        [add_out, sink],
        NodeOutputType::F32,
    )?;

    // The RHS uses the float_const(...) builder.
    let apply = rewrite_rules! {
        FAdd(FloatConst(b), x) => float_const(b, NodeOutputType::F32),
    };

    let add_node = fg.graph.get_node_from_output(add_out);
    let res = apply(&mut fg, add_node)?;
    assert_eq!(res, OptimizationResult::Changed);

    Ok(())
}

// ── FMul / FDiv smoke ────────────────────────────────────────────────────────

/// `FMul` is commutative — a rule with `FMul(FloatConst(b), x)` should fire
/// when the constant is on the right.
#[test]
fn fmul_commutative_smoke() -> Result<()> {
    let mut fg = empty_fg();

    let x = fg.make_float_const(0x3f80_0000u64, NodeOutputType::F32)?;
    let two = fg.make_float_const(0x4000_0000u64, NodeOutputType::F32)?; // 2.0f32
    // FMul(x, two) — the constant is on the RIGHT, rule expects it on the LEFT.
    let mul_out = fg.make_value_node(
        NodeKind::FloatBinaryOp(FloatBinaryOp::Mul),
        [x, two],
        NodeOutputType::F32,
    )?;

    let sink = fg.make_float_const(3u64, NodeOutputType::F32)?;
    fg.make_value_node(
        NodeKind::FloatBinaryOp(FloatBinaryOp::Add),
        [mul_out, sink],
        NodeOutputType::F32,
    )?;

    // Rule with the constant on the LEFT — commutative matching should still fire.
    let apply = rewrite_rules! {
        FMul(FloatConst(b), x) => float_const(b, NodeOutputType::F32),
    };

    let mul_node = fg.graph.get_node_from_output(mul_out);
    let res = apply(&mut fg, mul_node)?;
    assert_eq!(res, OptimizationResult::Changed);

    Ok(())
}

/// `FDiv` is NOT commutative — the rule `FDiv(FloatConst(0), x)` should NOT
/// fire when the zero is on the RIGHT (i.e. it is `x / 0`, not `0 / x`).
#[test]
fn fdiv_not_commutative() -> Result<()> {
    let mut fg = empty_fg();

    let x = fg.make_float_const(0x3f80_0000u64, NodeOutputType::F32)?; // 1.0f32
    let zero = fg.make_float_const(0u64, NodeOutputType::F32)?;         // 0.0f32
    // FDiv(x, zero) — zero is on the RIGHT, rule requires it on the LEFT.
    let div_out = fg.make_value_node(
        NodeKind::FloatBinaryOp(FloatBinaryOp::Div),
        [x, zero], // x / 0, NOT 0 / x
        NodeOutputType::F32,
    )?;

    let sink = fg.make_float_const(0x4000_0000u64, NodeOutputType::F32)?; // 2.0
    fg.make_value_node(
        NodeKind::FloatBinaryOp(FloatBinaryOp::Add),
        [div_out, sink],
        NodeOutputType::F32,
    )?;

    // Rule: FDiv(FloatConst(0), x) — the ZERO must be the LEFT operand.
    // Since FDiv is non-commutative, it must NOT flip operands to match.
    let apply = rewrite_rules! {
        FDiv(FloatConst(0), x) => float_const(0u64, NodeOutputType::F32),
    };

    let div_node = fg.graph.get_node_from_output(div_out);
    let res = apply(&mut fg, div_node)?;
    assert_eq!(res, OptimizationResult::NoChange);

    Ok(())
}

// ── FLe smoke ────────────────────────────────────────────────────────────────

/// Smoke test that `FLe` parses and fires correctly (not commutative).
#[test]
fn fle_fires_correctly() -> Result<()> {
    let mut fg = empty_fg();

    let zero = fg.make_float_const(0u64, NodeOutputType::F32)?;
    let one = fg.make_float_const(0x3f80_0000u64, NodeOutputType::F32)?;
    // FLe(0.0, 1.0) — zero on left.
    let le_out = fg.make_value_node(
        NodeKind::FloatCmpOp(FloatCmpOp::LessEqual),
        [zero, one],
        NodeOutputType::Bool,
    )?;

    fg.make_value_node(
        NodeKind::BoolBinaryOp(ir::BoolBinaryOp::Or),
        [le_out, le_out],
        NodeOutputType::Bool,
    )?;

    let apply = rewrite_rules! {
        FLe(FloatConst(0), x) => bool_const(true),
    };

    let le_node = fg.graph.get_node_from_output(le_out);
    let res = apply(&mut fg, le_node)?;
    assert_eq!(res, OptimizationResult::Changed);

    Ok(())
}
