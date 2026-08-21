//! Three comparisons are lowered at lift time so `IntCmpOp` holds only
//! primitive predicates:
//!
//! - `IntNotEqual(a, b)` -> `Xor(IntEqual(a, b), IntConst(1)):I1`
//! - `IntLessEqual(a, b)` -> `Xor(IntLess(b, a), IntConst(1)):I1`
//! - `IntSlessEqual(a, b)` -> `Xor(IntSless(b, a), IntConst(1)):I1`
//!
//! Logical negation of an `I1` is `Xor(_, IntConst(1))`; bitwise complement is
//! `Xor(_, all_ones)` everywhere.

use strider_ir::{ExtendOp, IRBuilderExt, IntBinaryOp, IntCmpOp, IntUnaryOp, ValueType, VnTypeExt};

use crate::lift::FunctionLifter;
use crate::lift::pcode_util::{Result, nth_input_or_err, require_output_vn};

/// Sleigh's contract already guarantees equal widths here, and the IR builders
/// reject a mismatch anyway.  This exists to surface a malformed `.sla` spec
/// with a precise lift-time diagnostic instead of a generic builder error.
fn require_equal_input_widths(a: &rsleigh::Vn, b: &rsleigh::Vn) -> Result<()> {
    if a.size != b.size {
        return Err(anyhow::anyhow!(
            "p-code input width mismatch: lhs={} rhs={} (Sleigh requires equal widths)",
            a.size,
            b.size,
        ));
    }
    Ok(())
}

/// Unary counterpart of [`require_equal_input_widths`], same rationale.
pub(super) fn require_equal_input_output_width(
    input: &rsleigh::Vn,
    output: &rsleigh::Vn,
) -> Result<()> {
    if input.size != output.size {
        return Err(anyhow::anyhow!(
            "p-code unary op width mismatch: input={} output={} (Sleigh requires equal widths)",
            input.size,
            output.size,
        ));
    }
    Ok(())
}

/// All-ones constant of width `ty`, the RHS of a lowered bitwise complement.
fn build_all_ones(
    builder: &mut strider_ir::FunctionBuilder,
    ty: strider_ir::ValueType,
) -> Result<strider_ir::Value> {
    if ty.byte_size() <= 16 {
        // build_int_const masks u128::MAX down to the declared width.
        builder.build_int_const(u128::MAX, ty)
    } else if ty == ValueType::I256 {
        builder.build_int_const_limbs(&[u64::MAX; 4], ty)
    } else {
        builder.build_int_const_limbs(&[u64::MAX; 8], ty)
    }
}

/// Only the wider direction is rejected: a narrower operand extends correctly,
/// which is the intended semantics.
fn reject_operand_wider_than_output(operand: &rsleigh::Vn, output: &rsleigh::Vn) -> Result<()> {
    if operand.size > output.size {
        return Err(anyhow::anyhow!(
            "p-code signed op width mismatch: operand={} wider than output={} \
             (would silently truncate before the signed operation)",
            operand.size,
            output.size,
        ));
    }
    Ok(())
}

impl<'a, R: rsleigh::MemReader> FunctionLifter<'a, R> {
    pub(super) fn process_int_unary_op(
        &mut self,
        insn: &rsleigh::Insn,
        op: IntUnaryOp,
    ) -> Result<()> {
        self.lift_int_unary(insn, true, |b, v, t| b.build_int_unary_operation(v, op, t))
    }

    /// Sleigh's `IntNeg` is bitwise complement `~x`, which canonicalises to
    /// `Xor(x, all_ones)`.  [`build_all_ones`] routes the wide widths through
    /// the wide-const path, so a SIMD-wide complement (YMM, ZMM) lifts rather
    /// than erroring.
    pub(super) fn handle_int_neg_as_xor(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        self.lift_int_unary(insn, true, |b, v, t| {
            let all_ones = build_all_ones(b, t)?;
            b.build_int_binary_operation(v, all_ones, IntBinaryOp::Xor, t)
        })
    }

    /// Permissive about mixed input widths: real Sleigh on 64-bit arches
    /// legitimately mixes an 8-byte register with a 4-byte spill or immediate
    /// around integer-promotion boundaries, so each operand is coerced to the
    /// output width below.  Equal-width lift-time checks are reserved for the
    /// lowered forms, whose arithmetic requires them.
    pub(super) fn process_int_binary_op(
        &mut self,
        insn: &rsleigh::Insn,
        op: IntBinaryOp,
    ) -> Result<()> {
        // That coercion silently truncates an operand WIDER than the output.
        // Division and remainder low bits are not width-agnostic, so a
        // truncated operand corrupts the result: guard both operands.
        // `SShiftRight` sign-extends only the value; its shift count may
        // legally be any width (`sar reg, cl`), so only the value is guarded.
        let out_vn = require_output_vn(insn)?;
        match op {
            IntBinaryOp::Sdiv | IntBinaryOp::Srem | IntBinaryOp::Div | IntBinaryOp::Rem => {
                reject_operand_wider_than_output(nth_input_or_err(insn, 0)?, out_vn)?;
                reject_operand_wider_than_output(nth_input_or_err(insn, 1)?, out_vn)?;
            }
            IntBinaryOp::SShiftRight => {
                reject_operand_wider_than_output(nth_input_or_err(insn, 0)?, out_vn)?;
            }
            _ => {}
        }
        let lhs = self.read_input(insn, 0)?;
        let rhs = self.read_input(insn, 1)?;
        let out_ty = out_vn.int_type()?;
        // Signed ops must SIGN-extend a narrower operand.  `SShiftRight`
        // sign-extends only the value; its shift count is unsigned.  Everything
        // else zero-extends, correct for the bitwise / shift-left / unsigned
        // ops whose low-width result is sign-agnostic.  Equal widths, the
        // common case, make the coercion a no-op.
        let (lhs, rhs) = match op {
            IntBinaryOp::Sdiv | IntBinaryOp::Srem => (
                self.builder
                    .extend_if_needed(lhs, out_ty, ExtendOp::SignExtend)?,
                self.builder
                    .extend_if_needed(rhs, out_ty, ExtendOp::SignExtend)?,
            ),
            IntBinaryOp::SShiftRight => (
                self.builder
                    .extend_if_needed(lhs, out_ty, ExtendOp::SignExtend)?,
                self.builder.convert_to_int_if_needed(rhs, out_ty)?,
            ),
            _ => (
                self.builder.convert_to_int_if_needed(lhs, out_ty)?,
                self.builder.convert_to_int_if_needed(rhs, out_ty)?,
            ),
        };
        let result = self
            .builder
            .build_int_binary_operation(lhs, rhs, op, out_ty)?;
        self.write_vn(out_vn, result)
    }

    /// Compares at the MAX of the two input widths so neither operand is
    /// truncated, extending the narrower one sign-correctly for the predicate.
    pub(super) fn process_int_cmp_op(&mut self, insn: &rsleigh::Insn, op: IntCmpOp) -> Result<()> {
        let in0_size = nth_input_or_err(insn, 0)?.size;
        let in1_size = nth_input_or_err(insn, 1)?.size;
        // Carry / Scarry / Sborrow are width-RELATIVE: they report overflow of
        // THIS width, so widening their operands makes the flag constant-false
        // (a wider add never carries out of the narrow width).  Sleigh always
        // emits them equal-width; fail loud if not.  The value comparisons
        // legitimately take mixed widths, so they are unguarded.
        if matches!(op, IntCmpOp::Carry | IntCmpOp::Scarry | IntCmpOp::Sborrow) {
            require_equal_input_widths(nth_input_or_err(insn, 0)?, nth_input_or_err(insn, 1)?)?;
        }
        let lhs = self.read_input(insn, 0)?;
        let rhs = self.read_input(insn, 1)?;
        let out_vn = require_output_vn(insn)?;
        let cmp_width = strider_ir::ValueType::int_for_byte_size(in0_size.max(in1_size))?;
        let ext_op = match op {
            IntCmpOp::Sless | IntCmpOp::Scarry | IntCmpOp::Sborrow => ExtendOp::SignExtend,
            IntCmpOp::Equal | IntCmpOp::Less | IntCmpOp::Carry => ExtendOp::ZeroExtend,
        };
        let lhs = self.builder.extend_if_needed(lhs, cmp_width, ext_op)?;
        let rhs = self.builder.extend_if_needed(rhs, cmp_width, ext_op)?;
        let result = self
            .builder
            .build_int_cmp_operation(lhs, rhs, op, cmp_width)?;
        self.write_vn(out_vn, result)
    }

    /// Shared lowering for the three negated comparisons.  The comparison runs
    /// at the INPUT width; the output is a 1-bit `I1`.  `swap_operands` covers
    /// the `a <= b` iff `not(b < a)` rewrite.
    fn lower_cmp_negated(
        &mut self,
        insn: &rsleigh::Insn,
        op: IntCmpOp,
        swap_operands: bool,
    ) -> Result<()> {
        require_equal_input_widths(nth_input_or_err(insn, 0)?, nth_input_or_err(insn, 1)?)?;
        let lhs = self.read_input(insn, 0)?;
        let rhs = self.read_input(insn, 1)?;
        let out_vn = require_output_vn(insn)?;
        let cmp_width = nth_input_or_err(insn, 0)?.int_type()?;
        let lhs = self.builder.convert_to_int_if_needed(lhs, cmp_width)?;
        let rhs = self.builder.convert_to_int_if_needed(rhs, cmp_width)?;
        let (cmp_lhs, cmp_rhs) = if swap_operands {
            (rhs, lhs)
        } else {
            (lhs, rhs)
        };
        let cmp = self
            .builder
            .build_int_cmp_operation(cmp_lhs, cmp_rhs, op, cmp_width)?;
        let negated = self.build_logical_not(cmp)?;
        self.write_vn(out_vn, negated)
    }

    /// Canonical logical NOT: `Xor(x, IntConst(1)):I1`, since at `I1` the
    /// all-ones constant of a bitwise complement is `1`.  `x` must already be
    /// `I1`.  Shared by the boolean, integer-cmp and float-cmp lowerings.
    pub(super) fn build_logical_not(&mut self, x: strider_ir::Value) -> Result<strider_ir::Value> {
        let one = self.builder.build_boolean_const(true);
        self.builder
            .build_int_binary_operation(x, one, IntBinaryOp::Xor, strider_ir::ValueType::I1)
    }

    /// Lowers to `Xor(IntEqual(a, b), IntConst(1)):I1`.
    pub(super) fn handle_int_not_equal(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        self.lower_cmp_negated(insn, IntCmpOp::Equal, false)
    }

    /// Lowers to `Xor(IntLess(b, a), IntConst(1)):I1`.
    pub(super) fn handle_int_less_equal(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        self.lower_cmp_negated(insn, IntCmpOp::Less, true)
    }

    /// Lowers to `Xor(IntSless(b, a), IntConst(1)):I1`.
    pub(super) fn handle_int_sless_equal(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        self.lower_cmp_negated(insn, IntCmpOp::Sless, true)
    }

    /// Lowers `IntSub(a, b)` to `Add(a, Neg(b))`, the one canonical shape
    /// patterns see.  Exact: `a - b` equals `a + (-b)` mod 2^W, and `Add`
    /// wraps identically.
    pub(super) fn handle_int_sub(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        // Sleigh requires all three widths to agree here, unlike the
        // comparison lowerings, whose I1 output makes only the input check
        // apply.
        require_equal_input_widths(nth_input_or_err(insn, 0)?, nth_input_or_err(insn, 1)?)?;
        let lhs = self.read_input(insn, 0)?;
        let rhs = self.read_input(insn, 1)?;
        let out_vn = require_output_vn(insn)?;
        let out_ty = out_vn.int_type()?;
        require_equal_input_output_width(nth_input_or_err(insn, 0)?, out_vn)?;
        let lhs = self.builder.convert_to_int_if_needed(lhs, out_ty)?;
        let rhs = self.builder.convert_to_int_if_needed(rhs, out_ty)?;
        let neg_rhs = self
            .builder
            .build_int_unary_operation(rhs, IntUnaryOp::Neg, out_ty)?;
        let sum =
            self.builder
                .build_int_binary_operation(lhs, neg_rhs, IntBinaryOp::Add, out_ty)?;
        self.write_vn(out_vn, sum)
    }
}
