//! Per-opcode-family value lifters.
//!
//! Each submodule provides one or more handlers that map a specific
//! pcode opcode (or family of related opcodes) onto IR builder calls.
//! The top-level dispatch lives in [`lift`].

use ir::{
    BoolBinaryOp, BoolUnaryOp, ExtendOp, FloatBinaryOp, FloatCmpOp, FloatUnaryOp, IntBinaryOp,
    IntCmpOp, IntUnaryOp,
};
use rsleigh::Opcode;

use crate::{Result, ValueLifter};

mod arithmetic;
mod boolean;
mod cast;
mod float;
mod integer;
mod mem_load;
mod misc_value;

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
        Opcode::BoolNeg => {
            lifter.process_bool_unary_op(insn, BoolUnaryOp::Neg)?;
        }
        Opcode::BoolAnd => {
            lifter.process_bool_binary_op(insn, BoolBinaryOp::And)?;
        }
        Opcode::BoolOr => {
            lifter.process_bool_binary_op(insn, BoolBinaryOp::Or)?;
        }
        Opcode::BoolXor => {
            lifter.process_bool_binary_op(insn, BoolBinaryOp::Xor)?;
        }
        Opcode::Copy => {
            lifter.handle_copy(insn)?;
        }
        Opcode::IntZext => {
            lifter.process_extend(insn, ExtendOp::ZeroExtend)?;
        }
        Opcode::IntSext => {
            lifter.process_extend(insn, ExtendOp::SignExtend)?;
        }
        // rsleigh's `Int2Comp` opcode is two's-complement negate (`-x`) → IR's `IntUnaryOp::Neg`.
        // rsleigh's `IntNeg` opcode is bitwise complement (`~x`) → IR's `IntUnaryOp::BitNot`.
        // The Sleigh nomenclature for these is reversed from conventional usage:
        // see `IntUnaryOp` doc-comment for the full naming-convention note.
        Opcode::Int2Comp => lifter.process_int_unary_op(insn, IntUnaryOp::Neg)?,
        Opcode::IntNeg => lifter.process_int_unary_op(insn, IntUnaryOp::BitNot)?,
        Opcode::IntAdd => lifter.process_int_binary_op(insn, IntBinaryOp::Add)?,
        Opcode::IntAnd => lifter.process_int_binary_op(insn, IntBinaryOp::And)?,
        Opcode::IntXor => lifter.process_int_binary_op(insn, IntBinaryOp::Xor)?,
        Opcode::IntOr => lifter.process_int_binary_op(insn, IntBinaryOp::Or)?,
        Opcode::IntDiv => lifter.process_int_binary_op(insn, IntBinaryOp::Div)?,
        Opcode::IntSdiv => lifter.process_int_binary_op(insn, IntBinaryOp::Sdiv)?,
        Opcode::IntMul => lifter.process_int_binary_op(insn, IntBinaryOp::Mul)?,
        Opcode::IntRight => lifter.process_int_binary_op(insn, IntBinaryOp::ShiftRight)?,
        Opcode::IntSright => lifter.process_int_binary_op(insn, IntBinaryOp::SShiftRight)?,
        Opcode::IntLeft => lifter.process_int_binary_op(insn, IntBinaryOp::ShiftLeft)?,
        Opcode::IntCarry => lifter.process_int_cmp_op(insn, IntCmpOp::Carry)?,
        Opcode::IntEqual => lifter.process_int_cmp_op(insn, IntCmpOp::Equal)?,
        Opcode::IntLess => lifter.process_int_cmp_op(insn, IntCmpOp::Less)?,
        Opcode::IntSless => lifter.process_int_cmp_op(insn, IntCmpOp::Sless)?,
        Opcode::IntLessEqual => lifter.handle_int_less_equal(insn)?,
        Opcode::IntRem => lifter.process_int_binary_op(insn, IntBinaryOp::Rem)?,
        Opcode::IntSrem => lifter.process_int_binary_op(insn, IntBinaryOp::Srem)?,
        Opcode::IntScarry => lifter.process_int_cmp_op(insn, IntCmpOp::Scarry)?,
        Opcode::IntSborrow => lifter.process_int_cmp_op(insn, IntCmpOp::Sborrow)?,
        Opcode::IntSlessEqual => lifter.handle_int_sless_equal(insn)?,
        Opcode::IntSub => lifter.process_int_binary_op(insn, IntBinaryOp::Sub)?,
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
        // ── Float arithmetic ──────────────────────────────────────────────
        Opcode::FloatAdd => lifter.process_float_binary_op(insn, FloatBinaryOp::Add)?,
        Opcode::FloatSub => lifter.process_float_binary_op(insn, FloatBinaryOp::Sub)?,
        Opcode::FloatMul => lifter.process_float_binary_op(insn, FloatBinaryOp::Mul)?,
        Opcode::FloatDiv => lifter.process_float_binary_op(insn, FloatBinaryOp::Div)?,
        // ── Float unary (float → float) ───────────────────────────────────
        Opcode::FloatNeg => lifter.process_float_unary_op(insn, FloatUnaryOp::Neg)?,
        Opcode::FloatAbs => lifter.process_float_unary_op(insn, FloatUnaryOp::Abs)?,
        Opcode::FloatSqrt => lifter.process_float_unary_op(insn, FloatUnaryOp::Sqrt)?,
        Opcode::FloatCeil => lifter.process_float_unary_op(insn, FloatUnaryOp::Ceil)?,
        Opcode::FloatFloor => lifter.process_float_unary_op(insn, FloatUnaryOp::Floor)?,
        Opcode::FloatRound => lifter.process_float_unary_op(insn, FloatUnaryOp::Round)?,
        // ── Float comparisons (→ bool) ────────────────────────────────────
        Opcode::FloatEqual => lifter.process_float_cmp_op(insn, FloatCmpOp::Equal)?,
        Opcode::FloatNotEqual => lifter.process_float_cmp_op(insn, FloatCmpOp::NotEqual)?,
        Opcode::FloatLess => lifter.process_float_cmp_op(insn, FloatCmpOp::Less)?,
        Opcode::FloatLessEqual => lifter.process_float_cmp_op(insn, FloatCmpOp::LessEqual)?,
        // FloatNan: tests whether input is NaN (unary, → bool).  Lowered to
        // FloatCmpOp::NotEqual(x, x) since IEEE 754 guarantees NaN != NaN.
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
        // Other opcodes are not yet handled by the lifter — the caller
        // routes them through its own dispatch.
        _ => return Ok(false),
    }
    Ok(true)
}
