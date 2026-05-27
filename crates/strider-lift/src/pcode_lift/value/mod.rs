//! Per-opcode-family value lifters.
//!
//! Each submodule provides one or more handlers that map a specific
//! pcode opcode (or family of related opcodes) onto IR builder calls.
//! The top-level dispatch lives in the parent `lift` module.

use strider_ir::{
    ExtendOp, FloatBinaryOp, FloatCmpOp, FloatUnaryOp, IntBinaryOp, IntCmpOp, IntUnaryOp,
};
use rsleigh::Opcode;

use crate::pcode_lift::{Result, ValueLifter};

mod arithmetic;
mod boolean;
mod cast;
mod float;
mod integer;
mod mem_load;
mod misc_value;

/// (Opcode, IntBinaryOp) dispatch table for the trivial arms that just
/// forward to `process_int_binary_op`.
static OPCODE_TO_INT_BINARY: &[(Opcode, IntBinaryOp)] = &[
    (Opcode::IntAdd, IntBinaryOp::Add),
    (Opcode::IntAnd, IntBinaryOp::And),
    (Opcode::IntXor, IntBinaryOp::Xor),
    (Opcode::IntOr, IntBinaryOp::Or),
    (Opcode::IntDiv, IntBinaryOp::Div),
    (Opcode::IntSdiv, IntBinaryOp::Sdiv),
    (Opcode::IntMul, IntBinaryOp::Mul),
    (Opcode::IntRight, IntBinaryOp::ShiftRight),
    (Opcode::IntSright, IntBinaryOp::SShiftRight),
    (Opcode::IntLeft, IntBinaryOp::ShiftLeft),
    (Opcode::IntRem, IntBinaryOp::Rem),
    (Opcode::IntSrem, IntBinaryOp::Srem),
];

/// (Opcode, IntCmpOp) dispatch table for the trivial arms that just
/// forward to `process_int_cmp_op`.
static OPCODE_TO_INT_CMP: &[(Opcode, IntCmpOp)] = &[
    (Opcode::IntCarry, IntCmpOp::Carry),
    (Opcode::IntEqual, IntCmpOp::Equal),
    (Opcode::IntLess, IntCmpOp::Less),
    (Opcode::IntSless, IntCmpOp::Sless),
    (Opcode::IntScarry, IntCmpOp::Scarry),
    (Opcode::IntSborrow, IntCmpOp::Sborrow),
];

/// (Opcode, IntUnaryOp) dispatch table.  Note the Sleigh nomenclature
/// reversal: rsleigh's `Int2Comp` is two's-complement negate (`-x`) →
/// IR's `IntUnaryOp::Neg`; rsleigh's `IntNeg` is bitwise complement
/// (`~x`) → IR's `IntUnaryOp::BitNot`.  See `IntUnaryOp` doc-comment.
static OPCODE_TO_INT_UNARY: &[(Opcode, IntUnaryOp)] = &[
    (Opcode::Int2Comp, IntUnaryOp::Neg),
    (Opcode::IntNeg, IntUnaryOp::BitNot),
];

/// (Opcode, ExtendOp) dispatch table for the trivial extend arms.
static OPCODE_TO_EXTEND: &[(Opcode, ExtendOp)] = &[
    (Opcode::IntZext, ExtendOp::ZeroExtend),
    (Opcode::IntSext, ExtendOp::SignExtend),
];

/// (Opcode, IntBinaryOp) dispatch table for the boolean binary opcodes.
/// Booleans are `I1`, so logical and/or/xor are integer and/or/xor at `I1`.
static OPCODE_TO_BOOL_BINARY: &[(Opcode, IntBinaryOp)] = &[
    (Opcode::BoolAnd, IntBinaryOp::And),
    (Opcode::BoolOr, IntBinaryOp::Or),
    (Opcode::BoolXor, IntBinaryOp::Xor),
];

/// (Opcode, IntUnaryOp) dispatch table for the boolean unary opcode.
/// Logical not of a 1-bit value is bitwise-not at `I1`.
static OPCODE_TO_BOOL_UNARY: &[(Opcode, IntUnaryOp)] = &[(Opcode::BoolNeg, IntUnaryOp::BitNot)];

/// (Opcode, FloatBinaryOp) dispatch table.
static OPCODE_TO_FLOAT_BINARY: &[(Opcode, FloatBinaryOp)] = &[
    (Opcode::FloatAdd, FloatBinaryOp::Add),
    (Opcode::FloatMul, FloatBinaryOp::Mul),
    (Opcode::FloatDiv, FloatBinaryOp::Div),
];

/// (Opcode, FloatUnaryOp) dispatch table.
static OPCODE_TO_FLOAT_UNARY: &[(Opcode, FloatUnaryOp)] = &[
    (Opcode::FloatNeg, FloatUnaryOp::Neg),
    (Opcode::FloatAbs, FloatUnaryOp::Abs),
    (Opcode::FloatSqrt, FloatUnaryOp::Sqrt),
    (Opcode::FloatCeil, FloatUnaryOp::Ceil),
    (Opcode::FloatFloor, FloatUnaryOp::Floor),
    (Opcode::FloatRound, FloatUnaryOp::Round),
];

/// (Opcode, FloatCmpOp) dispatch table.
static OPCODE_TO_FLOAT_CMP: &[(Opcode, FloatCmpOp)] = &[
    (Opcode::FloatEqual, FloatCmpOp::Equal),
    (Opcode::FloatLess, FloatCmpOp::Less),
];

fn lookup<T: Copy>(table: &[(Opcode, T)], op: Opcode) -> Option<T> {
    table.iter().find(|(o, _)| *o == op).map(|(_, t)| *t)
}

/// Dispatches `insn` to the appropriate per-opcode handler.
///
/// Returns `Ok(true)` when the opcode is value-producing and was
/// lifted; `Ok(false)` when the opcode is a control-flow / call /
/// store op the caller must handle itself.
pub(crate) fn lift<R: rsleigh::MemReader>(
    lifter: &mut ValueLifter<'_, R>,
    insn: &rsleigh::Insn,
) -> Result<bool> {
    match insn.opcode {
        Opcode::Copy => {
            lifter.handle_copy(insn)?;
        }
        Opcode::IntSub => lifter.handle_int_sub(insn)?,
        Opcode::IntLessEqual => lifter.handle_int_less_equal(insn)?,
        Opcode::IntSlessEqual => lifter.handle_int_sless_equal(insn)?,
        Opcode::IntNotEqual => lifter.handle_int_not_equal(insn)?,
        Opcode::Subpiece => lifter.handle_subpiece(insn)?,
        Opcode::Popcount => lifter.handle_popcount(insn)?,
        Opcode::Lzcount => lifter.handle_lzcount(insn)?,
        Opcode::Piece => lifter.handle_piece(insn)?,
        Opcode::Extract => lifter.handle_extract(insn)?,
        Opcode::Insert => lifter.handle_insert(insn)?,
        // PtrAdd: out = base + index * elem_size  (elem_size is a CONST input)
        Opcode::PtrAdd => lifter.handle_ptr_add(insn)?,
        // PtrSub: out = base - index
        Opcode::PtrSub => lifter.handle_ptr_sub(insn)?,
        // Cast: apply a data-type to the output varnode.  GHIDRA docs:
        // "semantically equivalent to a COPY operation".
        Opcode::Cast => lifter.handle_cast(insn)?,
        Opcode::FloatSub => lifter.handle_float_sub(insn)?,
        Opcode::FloatNotEqual => lifter.handle_float_not_equal(insn)?,
        Opcode::FloatLessEqual => lifter.handle_float_less_equal(insn)?,
        // FloatNan: tests whether input is NaN (unary, → bool).  Lowered to
        // BoolNeg(FloatEqual(x, x)) since IEEE 754 guarantees NaN != NaN.
        Opcode::FloatNan => lifter.handle_float_nan(insn)?,
        // ── Float / integer conversions ───────────────────────────────────
        Opcode::FloatInt2Float => lifter.handle_float_int_to_float(insn)?,
        Opcode::FloatFloat2Float => lifter.handle_float_float_to_float(insn)?,
        Opcode::FloatTrunc => lifter.handle_float_trunc(insn)?,
        Opcode::Load => lifter.handle_load(insn)?,
        // SegmentOp: segmented-address lookup.
        Opcode::SegmentOp => lifter.handle_segment_op(insn)?,
        // CPoolRef: JVM constant-pool lookup.  Opaque, variadic refs.
        Opcode::CPoolRef => lifter.handle_cpool_ref(insn)?,
        // New: JVM object allocation.  Opaque.
        Opcode::New => lifter.handle_new(insn)?,
        // Trivial table-driven arms: integer / bool / float binary,
        // comparison, unary, and extend ops that simply forward to a
        // `process_*_op` builder call.  Adding a new opcode in this
        // family is a one-row edit to the corresponding table above.
        other => {
            if let Some(op) = lookup(OPCODE_TO_INT_BINARY, other) {
                lifter.process_int_binary_op(insn, op)?;
            } else if let Some(op) = lookup(OPCODE_TO_INT_CMP, other) {
                lifter.process_int_cmp_op(insn, op)?;
            } else if let Some(op) = lookup(OPCODE_TO_INT_UNARY, other) {
                lifter.process_int_unary_op(insn, op)?;
            } else if let Some(op) = lookup(OPCODE_TO_EXTEND, other) {
                lifter.process_extend(insn, op)?;
            } else if let Some(op) = lookup(OPCODE_TO_BOOL_BINARY, other) {
                lifter.process_bool_binary_op(insn, op)?;
            } else if let Some(op) = lookup(OPCODE_TO_BOOL_UNARY, other) {
                lifter.process_bool_unary_op(insn, op)?;
            } else if let Some(op) = lookup(OPCODE_TO_FLOAT_BINARY, other) {
                lifter.process_float_binary_op(insn, op)?;
            } else if let Some(op) = lookup(OPCODE_TO_FLOAT_UNARY, other) {
                lifter.process_float_unary_op(insn, op)?;
            } else if let Some(op) = lookup(OPCODE_TO_FLOAT_CMP, other) {
                lifter.process_float_cmp_op(insn, op)?;
            } else {
                // Other opcodes are not yet handled by the lifter — the
                // caller routes them through its own dispatch.
                return Ok(false);
            }
        }
    }
    Ok(true)
}
