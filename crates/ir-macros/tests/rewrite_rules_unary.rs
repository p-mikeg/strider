//! TDD tests for `rewrite_rules!` unary node kinds:
//! `Truncate`, `Popcount`, `Lzcount`, `CastToBool`, `CastToInt`.
//!
//! Each test builds a minimal `BuiltFunctionGraph` containing the target node
//! applied to a constant, invokes a trivial rule that rewrites it to a
//! constant, and asserts `OptimizationResult::Changed`.

#![allow(
    clippy::panic,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::unreachable
)]

use ir::node::{NodeKind, NodeOutputType};
use ir::{BuiltFunctionGraph, FunctionBuilder};
use ir_macros::rewrite_rules;
use opt::OptimizationResult;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// Build a minimal valid `BuiltFunctionGraph` we can attach orphan nodes to.
fn empty_fg() -> BuiltFunctionGraph {
    let mut b = FunctionBuilder::new(vec![], &[], &[], &[]).unwrap();
    let region = b.create_region().unwrap();
    b.set_entry_region(region).unwrap();
    b.set_region(region);
    let v = b.build_int_const(0, NodeOutputType::U32);
    b.build_return(Some(v), &[]).unwrap();
    b.build().unwrap()
}

// ── Popcount ──────────────────────────────────────────────────────────────────

/// `Popcount(IntConst(v)) => int_const(v.count_ones() as u64, ty)`
/// We need to bypass the builder's constant-folding, so we use `make_value_node`
/// to attach the Popcount node directly.
#[test]
fn popcount_of_const_rewrites() -> Result<()> {
    let mut fg = empty_fg();

    // Build an IntConst(0xF0) of type U8.
    let c = fg.make_int_const(0xF0u64, NodeOutputType::U8)?;

    // Attach Popcount(c) : U8 directly (bypasses builder's fold).
    let pop_out = fg.make_value_node(NodeKind::Popcount, [c], NodeOutputType::U8)?;

    // Give pop_out a user so replace_all_uses finds something to redirect.
    let sink = fg.make_int_const(0u64, NodeOutputType::U8)?;
    fg.make_value_node(NodeKind::IntBinaryOp(ir::IntBinaryOp::Or), [pop_out, sink], NodeOutputType::U8)?;

    // Use a fixed constant on the RHS — this is a grammar test, not a semantic test.
    let apply = rewrite_rules! {
        Popcount(IntConst(v)) => int_const(4, ty),
    };

    let pop_node = fg.graph.get_node_from_output(pop_out);
    let res = apply(&mut fg, pop_node)?;
    assert_eq!(res, OptimizationResult::Changed);

    // pop_out should have no users now.
    assert!(
        fg.graph.output_use_cursor(pop_out).current().is_none(),
        "Popcount output should have no users after rewrite"
    );

    Ok(())
}

// ── Truncate ──────────────────────────────────────────────────────────────────

/// `Truncate(IntConst(v)) => int_const(v, ty)`
#[test]
fn truncate_of_const_rewrites() -> Result<()> {
    let mut fg = empty_fg();

    // Wide constant: U64
    let c = fg.make_int_const(0xDEAD_BEEF_CAFE_BABEu64, NodeOutputType::U64)?;

    // Truncate to U8.
    let trunc_out = fg.make_value_node(NodeKind::Truncate, [c], NodeOutputType::U8)?;

    let sink = fg.make_int_const(0u64, NodeOutputType::U8)?;
    fg.make_value_node(NodeKind::IntBinaryOp(ir::IntBinaryOp::Or), [trunc_out, sink], NodeOutputType::U8)?;

    let apply = rewrite_rules! {
        Truncate(IntConst(v)) => int_const(v, ty),
    };

    let trunc_node = fg.graph.get_node_from_output(trunc_out);
    let res = apply(&mut fg, trunc_node)?;
    assert_eq!(res, OptimizationResult::Changed);

    assert!(
        fg.graph.output_use_cursor(trunc_out).current().is_none(),
        "Truncate output should have no users after rewrite"
    );

    Ok(())
}

// ── Lzcount ───────────────────────────────────────────────────────────────────

/// `Lzcount(IntConst(v)) => int_const(v.leading_zeros() as u64, ty)`
#[test]
fn lzcount_of_const_rewrites() -> Result<()> {
    let mut fg = empty_fg();

    let c = fg.make_int_const(0x0F00u64, NodeOutputType::U16)?;
    let lz_out = fg.make_value_node(NodeKind::Lzcount, [c], NodeOutputType::U16)?;

    let sink = fg.make_int_const(0u64, NodeOutputType::U16)?;
    fg.make_value_node(NodeKind::IntBinaryOp(ir::IntBinaryOp::Or), [lz_out, sink], NodeOutputType::U16)?;

    // Use a fixed constant on the RHS — grammar test, not a semantic test.
    let apply = rewrite_rules! {
        Lzcount(IntConst(v)) => int_const(4, ty),
    };

    let lz_node = fg.graph.get_node_from_output(lz_out);
    let res = apply(&mut fg, lz_node)?;
    assert_eq!(res, OptimizationResult::Changed);

    assert!(
        fg.graph.output_use_cursor(lz_out).current().is_none(),
        "Lzcount output should have no users after rewrite"
    );

    Ok(())
}

// ── CastToBool ───────────────────────────────────────────────────────────────

/// `CastToBool(IntConst(v)) => bool_const(v != 0)`
/// CastToBool accepts `AnyValue` input and produces `Bool`.
#[test]
fn cast_to_bool_of_const_rewrites() -> Result<()> {
    let mut fg = empty_fg();

    // Use a non-zero integer constant.
    let c = fg.make_int_const(42u64, NodeOutputType::U32)?;
    let cb_out = fg.make_value_node(NodeKind::CastToBool, [c], NodeOutputType::Bool)?;

    // Give it a user (BoolAnd with itself).
    fg.make_value_node(
        NodeKind::BoolBinaryOp(ir::BoolBinaryOp::And),
        [cb_out, cb_out],
        NodeOutputType::Bool,
    )?;

    let apply = rewrite_rules! {
        CastToBool(IntConst(v)) => bool_const(v != 0),
    };

    let cb_node = fg.graph.get_node_from_output(cb_out);
    let res = apply(&mut fg, cb_node)?;
    assert_eq!(res, OptimizationResult::Changed);

    assert!(
        fg.graph.output_use_cursor(cb_out).current().is_none(),
        "CastToBool output should have no users after rewrite"
    );

    Ok(())
}

// ── CastToInt ────────────────────────────────────────────────────────────────

/// `CastToInt(IntConst(v)) => int_const(v, ty)`
/// CastToInt accepts `AnyValue` and produces `AnyInt`.
#[test]
fn cast_to_int_of_const_rewrites() -> Result<()> {
    let mut fg = empty_fg();

    // Feed it a float constant so a real CastToInt node is needed
    // (int→int would be folded to identity by the builder).
    let c = fg.make_float_const(0x3F80_0000u64, NodeOutputType::F32)?; // 1.0f32
    let ci_out = fg.make_value_node(NodeKind::CastToInt, [c], NodeOutputType::U32)?;

    let sink = fg.make_int_const(0u64, NodeOutputType::U32)?;
    fg.make_value_node(
        NodeKind::IntBinaryOp(ir::IntBinaryOp::Or),
        [ci_out, sink],
        NodeOutputType::U32,
    )?;

    // Rule: CastToInt of a float-const is a grammar test; the RHS is a dummy int_const.
    let apply = rewrite_rules! {
        CastToInt(FloatConst(bits)) => int_const(bits, ty),
    };

    let ci_node = fg.graph.get_node_from_output(ci_out);
    let res = apply(&mut fg, ci_node)?;
    assert_eq!(res, OptimizationResult::Changed);

    assert!(
        fg.graph.output_use_cursor(ci_out).current().is_none(),
        "CastToInt output should have no users after rewrite"
    );

    Ok(())
}
