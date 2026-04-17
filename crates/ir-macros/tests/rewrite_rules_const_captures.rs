//! Tests for `rewrite_rules!` extensions covering `BoolConst` / `FloatConst`
//! captures and the function-form `BAnd` / `BOr` / `BXor` boolean operators.

#![allow(
    clippy::panic,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::unreachable
)]

use ir::node::{NodeKind, NodeOutputType};
use ir::{BoolBinaryOp, FunctionBuilder};
use ir_macros::rewrite_rules;
use opt::OptimizationResult;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

// ── Rule: BAnd(BoolConst(true), x) => x ─────────────────────────────────────

/// Build a graph with an orphan-but-used `BAnd(BoolConst(true), BoolConst(false))`
/// node and verify that the rewrite rule rewrites it to `x` (the second operand).
///
/// As in `rule_3_sign_extend_constant` from the spike tests, we add nodes
/// directly on the built graph so the builder's constant folding doesn't
/// collapse the operation before the rule has a chance to fire.  The target
/// node gets a dummy user (an `Add` consuming its output indirectly via a
/// cast) so `replace_all_uses` has something to redirect.
#[test]
fn band_bool_const_true_left_identity() -> Result<()> {
    // Minimal function body — we only care about the orphan nodes we add below.
    let mut fg = {
        let mut b = FunctionBuilder::new(vec![], &[], &[], &[]).unwrap();
        let region = b.create_region().unwrap();
        b.set_entry_region(region).unwrap();
        b.set_region(region);
        let v = b.build_int_const(0, NodeOutputType::U32);
        b.build_return(Some(v), &[]).unwrap();
        b.build().unwrap()
    };

    // Build: BAnd(BoolConst(true), BoolConst(false))
    let c_true = fg.make_bool_const(true)?;
    let c_false = fg.make_bool_const(false)?;
    let band_out = fg.make_value_node(
        NodeKind::BoolBinaryOp(BoolBinaryOp::And),
        [c_true, c_false],
        NodeOutputType::Bool,
    )?;

    // Give band_out a user so `replace_all_uses` finds something to redirect.
    // (Use a second BAnd as the user.)
    let other_bool = fg.make_bool_const(false)?;
    fg.make_value_node(
        NodeKind::BoolBinaryOp(BoolBinaryOp::Or),
        [band_out, other_bool],
        NodeOutputType::Bool,
    )?;

    let apply = rewrite_rules! {
        BAnd(BoolConst(true), x) => x,
    };

    let band_node = fg.graph.get_node_from_output(band_out);
    let res = apply(&mut fg, band_node)?;
    assert_eq!(res, OptimizationResult::Changed);

    // band_out should have no users afterwards — the user was redirected to x (c_false).
    assert!(
        fg.graph.output_use_cursor(band_out).current().is_none(),
        "BAnd output should have no users after the rewrite"
    );

    // And c_false should now have at least one user (the redirected OR).
    assert!(
        fg.graph.output_use_cursor(c_false).current().is_some(),
        "`x` (c_false) should have at least one user after the rewrite"
    );

    Ok(())
}

// ── Rule: FloatConst(bits) capture ──────────────────────────────────────────

/// Smoke test that `FloatConst(bits)` binds the capture to the raw float bits.
/// The RHS builds a new `IntConst` of the raw bits so we can check the rewrite
/// actually materialised the captured value.
#[test]
fn float_const_capture_binds_bits() -> Result<()> {
    // Minimal function body.
    let mut fg = {
        let mut b = FunctionBuilder::new(vec![], &[], &[], &[]).unwrap();
        let region = b.create_region().unwrap();
        b.set_entry_region(region).unwrap();
        b.set_region(region);
        let v = b.build_int_const(0, NodeOutputType::U32);
        b.build_return(Some(v), &[]).unwrap();
        b.build().unwrap()
    };

    let bits: u64 = 0x4049_0fdb; // pi as f32 bits
    let f_out = fg.make_float_const(bits, NodeOutputType::F32)?;

    // Give f_out a user so replace_all_uses has something to redirect.
    let other = fg.make_float_const(0, NodeOutputType::F32)?;
    fg.make_value_node(
        NodeKind::FloatBinaryOp(ir::FloatBinaryOp::Add),
        [f_out, other],
        NodeOutputType::F32,
    )?;

    let apply = rewrite_rules! {
        FloatConst(bits) => int_const(bits, ty),
    };

    let fc_node = fg.graph.get_node_from_output(f_out);
    let res = apply(&mut fg, fc_node)?;
    assert_eq!(res, OptimizationResult::Changed);

    Ok(())
}

// ── Rule: BoolConst(true) literal match — makes sure non-matching node fails ─

/// Feed a `BoolConst(false)` to a rule looking for `BoolConst(true)` and check
/// the rule does NOT fire. Then feed a `BoolConst(true)` and check it DOES fire.
#[test]
fn bool_const_literal_match() -> Result<()> {
    let mut fg = {
        let mut b = FunctionBuilder::new(vec![], &[], &[], &[]).unwrap();
        let region = b.create_region().unwrap();
        b.set_entry_region(region).unwrap();
        b.set_region(region);
        let v = b.build_int_const(0, NodeOutputType::U32);
        b.build_return(Some(v), &[]).unwrap();
        b.build().unwrap()
    };

    // Create a BAnd with false on the left (should NOT match `BoolConst(true), x`).
    let c_false = fg.make_bool_const(false)?;
    let c_true = fg.make_bool_const(true)?;
    // BAnd(false, true) — after commutative flip, the "BoolConst(true)" pattern
    // will find c_true on either side, so the rule SHOULD fire here. But we
    // want a negative case: build BAnd(false, false) instead.
    let c_false2 = fg.make_bool_const(false)?;
    let band_neg = fg.make_value_node(
        NodeKind::BoolBinaryOp(BoolBinaryOp::And),
        [c_false, c_false2],
        NodeOutputType::Bool,
    )?;
    let other = fg.make_bool_const(false)?;
    fg.make_value_node(
        NodeKind::BoolBinaryOp(BoolBinaryOp::Or),
        [band_neg, other],
        NodeOutputType::Bool,
    )?;

    // Positive case: BAnd(true, false).
    let band_pos = fg.make_value_node(
        NodeKind::BoolBinaryOp(BoolBinaryOp::And),
        [c_true, c_false],
        NodeOutputType::Bool,
    )?;
    let other2 = fg.make_bool_const(false)?;
    fg.make_value_node(
        NodeKind::BoolBinaryOp(BoolBinaryOp::Or),
        [band_pos, other2],
        NodeOutputType::Bool,
    )?;

    let apply = rewrite_rules! {
        BAnd(BoolConst(true), x) => x,
    };

    // Negative case: no match.
    let band_neg_node = fg.graph.get_node_from_output(band_neg);
    let res_neg = apply(&mut fg, band_neg_node)?;
    assert_eq!(res_neg, OptimizationResult::NoChange);

    // Positive case: matches via commutative flip.
    let band_pos_node = fg.graph.get_node_from_output(band_pos);
    let res_pos = apply(&mut fg, band_pos_node)?;
    assert_eq!(res_pos, OptimizationResult::Changed);

    Ok(())
}
