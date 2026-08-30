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

/// `SShiftRight`'s value operand: the lowering agrees with
/// `OpBehaviorIntSright` only at equal widths, so anything else is rejected
/// rather than silently taken.
fn reject_operand_width_mismatch(operand: &rsleigh::Vn, output: &rsleigh::Vn) -> Result<()> {
    if operand.size != output.size {
        return Err(anyhow::anyhow!(
            "p-code signed shift width mismatch: operand={} against output={} \
             (the sign fill would land at the wrong width)",
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

    /// A shift count wider than the output, clamped so it still reads as
    /// out-of-range after truncation.
    ///
    /// P-code tests the FULL count against `8 * sizeout` (`opbehavior.cc`
    /// `OpBehaviorIntRight` / `IntSright` / `IntLeft`), so truncating
    /// `0x1_0000_0000` to `I32` yields 0 and the shift silently does nothing.
    /// x86 SIMD shift-by-register (`psrad xmm, xmm` and friends) is exactly
    /// this shape: a 4-byte lane shifted by the 8-byte count the ISA reads
    /// from `SRC[63:0]`, where an over-large count means "fill with zero / the
    /// sign bit".
    ///
    /// OR-ing `out_bits` in leaves every one of its bits set, so the result is
    /// `>= out_bits` whatever the truncation kept. That holds for the widths
    /// that are not powers of two (`I24`, `I48`, `I80`, ...) as well.
    fn saturate_shift_count(
        &mut self,
        count: strider_ir::Value,
        out_ty: ValueType,
    ) -> Result<strider_ir::Value> {
        let count_ty = strider_ir::IRViewer::value_type(self.builder.function(), count)?;
        if count_ty.bit_width() <= out_ty.bit_width() {
            return Ok(count);
        }
        let out_bits = u128::try_from(out_ty.bit_width()).expect("bit width fits u128");
        let bound = self.builder.build_int_const(out_bits, count_ty)?;
        let in_range = self.builder.build_int_cmp_operation(
            count,
            bound,
            strider_ir::IntCmpOp::Less,
            count_ty,
        )?;
        let one = self.builder.build_int_const(1u128, ValueType::I1)?;
        let too_big = self.builder.build_int_binary_operation(
            in_range,
            one,
            IntBinaryOp::Xor,
            ValueType::I1,
        )?;
        let widened = self
            .builder
            .extend_if_needed(too_big, out_ty, ExtendOp::ZeroExtend)?;
        let all_ones =
            self.builder
                .build_int_unary_operation(widened, strider_ir::IntUnaryOp::Neg, out_ty)?;
        let bound_out = self.builder.build_int_const(out_bits, out_ty)?;
        let addend = self.builder.build_int_binary_operation(
            all_ones,
            bound_out,
            IntBinaryOp::And,
            out_ty,
        )?;
        // Either direction: `Or` below pins both operands to `out_ty`, and a
        // Sleigh shift count is any width.
        let widened_count =
            self.builder
                .extend_if_needed(count, out_ty, strider_ir::ExtendOp::ZeroExtend)?;
        let narrow = self.builder.truncate_if_needed(widened_count, out_ty)?;
        self.builder
            .build_int_binary_operation(narrow, addend, IntBinaryOp::Or, out_ty)
    }

    /// Permissive about mixed input widths: real Sleigh on 64-bit arches
    /// legitimately mixes an 8-byte register with a 4-byte spill or immediate
    /// around integer-promotion boundaries, so each operand is coerced to the
    /// output width. Equal-width lift-time checks are reserved for the lowered
    /// forms, whose arithmetic requires them.
    pub(super) fn process_int_binary_op(
        &mut self,
        insn: &rsleigh::Insn,
        op: IntBinaryOp,
    ) -> Result<()> {
        // That coercion silently truncates an operand WIDER than the output.
        // Division and remainder low bits are not width-agnostic, so a
        // truncated operand corrupts the result: guard both operands.
        // `SShiftRight`'s value is guarded in BOTH directions, a narrower one
        // diverging from GHIDRA as well; its shift count may legally be any
        // width (`sar reg, cl`), so the count is not guarded at all.
        let out_vn = require_output_vn(insn)?;
        match op {
            IntBinaryOp::Sdiv | IntBinaryOp::Srem | IntBinaryOp::Div | IntBinaryOp::Rem => {
                reject_operand_wider_than_output(nth_input_or_err(insn, 0)?, out_vn)?;
                reject_operand_wider_than_output(nth_input_or_err(insn, 1)?, out_vn)?;
            }
            IntBinaryOp::SShiftRight => {
                reject_operand_width_mismatch(nth_input_or_err(insn, 0)?, out_vn)?;
            }
            _ => {}
        }
        let lhs = self.read_input(insn, 0)?;
        let rhs = self.read_input(insn, 1)?;
        let out_ty = out_vn.int_type()?;
        // Sdiv / Srem / SShiftRight SIGN-extend a narrower operand; every other op
        // zero-extends, correct for the bitwise / shift-left / unsigned ops
        // whose low-width result is sign-agnostic.  Equal widths, the common
        // case, make the coercion a no-op.
        //
        // `SShiftRight` sign-extends its value to the OUTPUT width and
        // zero-extends its count.  `OpBehaviorIntSright` instead computes the
        // sign-fill mask at the INPUT width and leaves the bits above it zero
        // (sizein=4/sizeout=8 over 0xFFFFFFFF gives GHIDRA 0xFFFFFFFF, this
        // lowering 0xFFFFFFFFFFFFFFFF), so the two agree only at equal widths,
        // which the guard above is what makes true.
        let rhs = match op {
            IntBinaryOp::ShiftLeft | IntBinaryOp::ShiftRight | IntBinaryOp::SShiftRight => {
                self.saturate_shift_count(rhs, out_ty)?
            }
            _ => rhs,
        };
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
