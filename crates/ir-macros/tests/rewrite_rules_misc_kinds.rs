//! TDD tests for `rewrite_rules!` grammar: Piece, Extract::<lsb,len>,
//! Insert::<lsb,len>, Extend::<ZeroExtend> (already-supported variant), and
//! the six int/float conversion kinds: IntToFloat, FloatToInt, FloatToFloat,
//! IntBitsToFloat, FloatBitsToInt, CastToFloat.
//!
//! These are grammar + codegen smoke tests — the rewrite values are not
//! semantically correct; we only verify the rule fires.

#![allow(
    clippy::panic,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::unreachable
)]

use ir::node::{NodeKind, NodeOutputType};
use ir::{BuiltFunctionGraph, ExtendOp, FunctionBuilder};
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

// ── Extend::<ZeroExtend> (already supported — regression / ZeroExtend variant) ───

#[test]
fn zero_extend_of_const_rewrites() -> Result<()> {
    let mut fg = empty_fg();

    let c = fg.make_int_const(0xABu64, NodeOutputType::U8)?;
    let ext_out = fg.make_value_node(
        NodeKind::Extend(ExtendOp::ZeroExtend),
        [c],
        NodeOutputType::U32,
    )?;

    // Give it a user.
    let sink = fg.make_int_const(0u64, NodeOutputType::U32)?;
    fg.make_value_node(
        NodeKind::IntBinaryOp(ir::IntBinaryOp::Or),
        [ext_out, sink],
        NodeOutputType::U32,
    )?;

    let apply = rewrite_rules! {
        Extend::<ZeroExtend>(IntConst(v) : in_ty) => int_const(v, ty),
    };

    let ext_node = fg.graph.get_node_from_output(ext_out);
    let res = apply(&mut fg, ext_node)?;
    assert_eq!(res, OptimizationResult::Changed);

    assert!(
        fg.graph.output_use_cursor(ext_out).current().is_none(),
        "ZeroExtend output should have no users after rewrite"
    );

    Ok(())
}

// ── Piece ─────────────────────────────────────────────────────────────────────

#[test]
fn piece_of_consts_rewrites() -> Result<()> {
    let mut fg = empty_fg();

    let hi = fg.make_int_const(0xABu64, NodeOutputType::U8)?;
    let lo = fg.make_int_const(0xCDu64, NodeOutputType::U8)?;

    // Piece(hi, lo) : U16
    let piece_out = fg.make_value_node(NodeKind::Piece, [hi, lo], NodeOutputType::U16)?;

    let sink = fg.make_int_const(0u64, NodeOutputType::U16)?;
    fg.make_value_node(
        NodeKind::IntBinaryOp(ir::IntBinaryOp::Or),
        [piece_out, sink],
        NodeOutputType::U16,
    )?;

    // Grammar smoke: rewrite to a constant (semantics irrelevant here).
    let apply = rewrite_rules! {
        Piece(IntConst(h), IntConst(l)) => int_const(42, ty),
    };

    let piece_node = fg.graph.get_node_from_output(piece_out);
    let res = apply(&mut fg, piece_node)?;
    assert_eq!(res, OptimizationResult::Changed);

    assert!(
        fg.graph.output_use_cursor(piece_out).current().is_none(),
        "Piece output should have no users after rewrite"
    );

    Ok(())
}

// ── Extract::<lsb, len> ───────────────────────────────────────────────────────

#[test]
fn extract_of_const_rewrites() -> Result<()> {
    let mut fg = empty_fg();

    let c = fg.make_int_const(0xABCDu64, NodeOutputType::U16)?;

    // Extract::<4, 4>(c) : U8 — bits 4..8 of the input.
    let ext_out = fg.make_value_node(NodeKind::Extract { lsb: 4, len: 4 }, [c], NodeOutputType::U8)?;

    let sink = fg.make_int_const(0u64, NodeOutputType::U8)?;
    fg.make_value_node(
        NodeKind::IntBinaryOp(ir::IntBinaryOp::Or),
        [ext_out, sink],
        NodeOutputType::U8,
    )?;

    let apply = rewrite_rules! {
        Extract::<4, 4>(IntConst(v)) => int_const(42, ty),
    };

    let ext_node = fg.graph.get_node_from_output(ext_out);
    let res = apply(&mut fg, ext_node)?;
    assert_eq!(res, OptimizationResult::Changed);

    assert!(
        fg.graph.output_use_cursor(ext_out).current().is_none(),
        "Extract output should have no users after rewrite"
    );

    Ok(())
}

// ── Insert::<lsb, len> ───────────────────────────────────────────────────────

#[test]
fn insert_of_consts_rewrites() -> Result<()> {
    let mut fg = empty_fg();

    let dest = fg.make_int_const(0xABCDu64, NodeOutputType::U16)?;
    let src  = fg.make_int_const(0x0Fu64,   NodeOutputType::U8)?;

    // Insert::<4, 4>(dest, src) : U16
    let ins_out = fg.make_value_node(
        NodeKind::Insert { lsb: 4, len: 4 },
        [dest, src],
        NodeOutputType::U16,
    )?;

    let sink = fg.make_int_const(0u64, NodeOutputType::U16)?;
    fg.make_value_node(
        NodeKind::IntBinaryOp(ir::IntBinaryOp::Or),
        [ins_out, sink],
        NodeOutputType::U16,
    )?;

    let apply = rewrite_rules! {
        Insert::<4, 4>(IntConst(d), IntConst(s)) => int_const(42, ty),
    };

    let ins_node = fg.graph.get_node_from_output(ins_out);
    let res = apply(&mut fg, ins_node)?;
    assert_eq!(res, OptimizationResult::Changed);

    assert!(
        fg.graph.output_use_cursor(ins_out).current().is_none(),
        "Insert output should have no users after rewrite"
    );

    Ok(())
}

// ── IntToFloat ────────────────────────────────────────────────────────────────

#[test]
fn int_to_float_of_const_rewrites() -> Result<()> {
    let mut fg = empty_fg();

    let c = fg.make_int_const(42u64, NodeOutputType::U32)?;

    // IntToFloat(c) : F64
    let itf_out = fg.make_value_node(NodeKind::IntToFloat, [c], NodeOutputType::F64)?;

    // Give it a float user.
    let sink = fg.make_float_const(0u64, NodeOutputType::F64)?;
    fg.make_value_node(
        NodeKind::FloatBinaryOp(ir::FloatBinaryOp::Add),
        [itf_out, sink],
        NodeOutputType::F64,
    )?;

    let apply = rewrite_rules! {
        IntToFloat(IntConst(v)) => float_const(0u64, NodeOutputType::F64),
    };

    let itf_node = fg.graph.get_node_from_output(itf_out);
    let res = apply(&mut fg, itf_node)?;
    assert_eq!(res, OptimizationResult::Changed);

    assert!(
        fg.graph.output_use_cursor(itf_out).current().is_none(),
        "IntToFloat output should have no users after rewrite"
    );

    Ok(())
}

// ── FloatToInt ────────────────────────────────────────────────────────────────

#[test]
fn float_to_int_of_const_rewrites() -> Result<()> {
    let mut fg = empty_fg();

    let c = fg.make_float_const(0x3F80_0000u64, NodeOutputType::F32)?; // 1.0f32

    // FloatToInt(c) : U32
    let fti_out = fg.make_value_node(NodeKind::FloatToInt, [c], NodeOutputType::U32)?;

    let sink = fg.make_int_const(0u64, NodeOutputType::U32)?;
    fg.make_value_node(
        NodeKind::IntBinaryOp(ir::IntBinaryOp::Or),
        [fti_out, sink],
        NodeOutputType::U32,
    )?;

    let apply = rewrite_rules! {
        FloatToInt(FloatConst(bits)) => int_const(0, ty),
    };

    let fti_node = fg.graph.get_node_from_output(fti_out);
    let res = apply(&mut fg, fti_node)?;
    assert_eq!(res, OptimizationResult::Changed);

    assert!(
        fg.graph.output_use_cursor(fti_out).current().is_none(),
        "FloatToInt output should have no users after rewrite"
    );

    Ok(())
}

// ── FloatToFloat ──────────────────────────────────────────────────────────────

#[test]
fn float_to_float_of_const_rewrites() -> Result<()> {
    let mut fg = empty_fg();

    let c = fg.make_float_const(0x3F80_0000u64, NodeOutputType::F32)?; // 1.0f32

    // FloatToFloat(c) : F64 — F32 → F64 conversion.
    let ftf_out = fg.make_value_node(NodeKind::FloatToFloat, [c], NodeOutputType::F64)?;

    let sink = fg.make_float_const(0u64, NodeOutputType::F64)?;
    fg.make_value_node(
        NodeKind::FloatBinaryOp(ir::FloatBinaryOp::Add),
        [ftf_out, sink],
        NodeOutputType::F64,
    )?;

    let apply = rewrite_rules! {
        FloatToFloat(FloatConst(bits)) => float_const(0u64, NodeOutputType::F64),
    };

    let ftf_node = fg.graph.get_node_from_output(ftf_out);
    let res = apply(&mut fg, ftf_node)?;
    assert_eq!(res, OptimizationResult::Changed);

    assert!(
        fg.graph.output_use_cursor(ftf_out).current().is_none(),
        "FloatToFloat output should have no users after rewrite"
    );

    Ok(())
}

// ── IntBitsToFloat ────────────────────────────────────────────────────────────

/// Note: `build_int_bits_to_float` immediately folds `IntConst → FloatConst`,
/// so we must use `make_value_node` to create an actual `IntBitsToFloat` node.
#[test]
fn int_bits_to_float_of_const_rewrites() -> Result<()> {
    let mut fg = empty_fg();

    // Use an IntConst as input — we bypass the builder's fold via make_value_node.
    let c = fg.make_int_const(0x3F80_0000u64, NodeOutputType::U32)?;

    // IntBitsToFloat(c) : F32
    let ibtf_out = fg.make_value_node(NodeKind::IntBitsToFloat, [c], NodeOutputType::F32)?;

    let sink = fg.make_float_const(0u64, NodeOutputType::F32)?;
    fg.make_value_node(
        NodeKind::FloatBinaryOp(ir::FloatBinaryOp::Add),
        [ibtf_out, sink],
        NodeOutputType::F32,
    )?;

    let apply = rewrite_rules! {
        IntBitsToFloat(IntConst(v)) => float_const(0u64, NodeOutputType::F32),
    };

    let ibtf_node = fg.graph.get_node_from_output(ibtf_out);
    let res = apply(&mut fg, ibtf_node)?;
    assert_eq!(res, OptimizationResult::Changed);

    assert!(
        fg.graph.output_use_cursor(ibtf_out).current().is_none(),
        "IntBitsToFloat output should have no users after rewrite"
    );

    Ok(())
}

// ── FloatBitsToInt ────────────────────────────────────────────────────────────

/// Note: `build_float_bits_to_int` immediately folds `FloatConst → IntConst`,
/// so we must use `make_value_node` to create an actual `FloatBitsToInt` node.
#[test]
fn float_bits_to_int_of_const_rewrites() -> Result<()> {
    let mut fg = empty_fg();

    let c = fg.make_float_const(0x3F80_0000u64, NodeOutputType::F32)?;

    // FloatBitsToInt(c) : U32
    let fbti_out = fg.make_value_node(NodeKind::FloatBitsToInt, [c], NodeOutputType::U32)?;

    let sink = fg.make_int_const(0u64, NodeOutputType::U32)?;
    fg.make_value_node(
        NodeKind::IntBinaryOp(ir::IntBinaryOp::Or),
        [fbti_out, sink],
        NodeOutputType::U32,
    )?;

    let apply = rewrite_rules! {
        FloatBitsToInt(FloatConst(bits)) => int_const(0, ty),
    };

    let fbti_node = fg.graph.get_node_from_output(fbti_out);
    let res = apply(&mut fg, fbti_node)?;
    assert_eq!(res, OptimizationResult::Changed);

    assert!(
        fg.graph.output_use_cursor(fbti_out).current().is_none(),
        "FloatBitsToInt output should have no users after rewrite"
    );

    Ok(())
}

// ── CastToFloat ───────────────────────────────────────────────────────────────

#[test]
fn cast_to_float_of_const_rewrites() -> Result<()> {
    let mut fg = empty_fg();

    let c = fg.make_int_const(42u64, NodeOutputType::U32)?;

    // CastToFloat(c) : F64
    let ctf_out = fg.make_value_node(NodeKind::CastToFloat, [c], NodeOutputType::F64)?;

    let sink = fg.make_float_const(0u64, NodeOutputType::F64)?;
    fg.make_value_node(
        NodeKind::FloatBinaryOp(ir::FloatBinaryOp::Add),
        [ctf_out, sink],
        NodeOutputType::F64,
    )?;

    let apply = rewrite_rules! {
        CastToFloat(IntConst(v)) => float_const(0u64, NodeOutputType::F64),
    };

    let ctf_node = fg.graph.get_node_from_output(ctf_out);
    let res = apply(&mut fg, ctf_node)?;
    assert_eq!(res, OptimizationResult::Changed);

    assert!(
        fg.graph.output_use_cursor(ctf_out).current().is_none(),
        "CastToFloat output should have no users after rewrite"
    );

    Ok(())
}
