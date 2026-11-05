//! BOOL ops read their input's low bit, truncating it to `I1` (p-code acts on
//! the whole byte); every vendored spec feeds them 0/1.

use strider_ir::node::ValueId;
use strider_ir::{
    ExtendOp, FunctionBuilder, IRBuilderExt, IRViewer, IntBinaryOp, IntCmpOp, IntUnaryOp,
    ValueType, VnTypeExt,
};

use anyhow::Result;

use crate::FunctionLifter;
use crate::pcode_util::{nth_input_or_err, require_output_vn};

/// Errs unless `a` and `b` have equal widths, as the Sleigh compiler
/// guarantees.
pub(super) fn require_same_size(a: &rsleigh::Vn, b: &rsleigh::Vn, what: &str) -> Result<()> {
    if a.size != b.size {
        return Err(anyhow::anyhow!(
            "p-code {what} width mismatch: {} against {} (Sleigh requires equal widths)",
            a.size,
            b.size,
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

    /// Sleigh's `IntNeg` is `~x`, lowered to `Xor(x, all_ones)`.
    pub(super) fn handle_int_neg_as_xor(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        self.lift_int_unary(insn, true, |b, v, t| {
            let all_ones = b.build_all_ones(t)?;
            b.build_int_binary_operation(v, all_ones, IntBinaryOp::Xor, t)
        })
    }

    /// Single-input integer op. With `enforce_equal_io_width` the input must
    /// have the output's width; off for `popcount` / `lzcount`, which count
    /// over the input's own width.
    pub(super) fn lift_int_unary(
        &mut self,
        insn: &rsleigh::Insn,
        enforce_equal_io_width: bool,
        build: impl FnOnce(&mut FunctionBuilder, ValueId, ValueType) -> Result<ValueId>,
    ) -> Result<()> {
        let out_vn = require_output_vn(insn)?;
        if enforce_equal_io_width {
            require_same_size(nth_input_or_err(insn, 0)?, out_vn, "unary op")?;
        }
        let value = self.read_input(insn, 0)?;
        let result = build(&mut self.builder, value, out_vn.int_type()?)?;
        self.write_vn(out_vn, result)
    }

    pub(super) fn handle_popcount(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        self.lift_int_unary(insn, false, |b, v, t| b.build_popcount(v, t))
    }

    pub(super) fn handle_lzcount(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        self.lift_int_unary(insn, false, |b, v, t| b.build_lzcount(v, t))
    }

    /// Narrows a shift count wider than the output so an out-of-range count
    /// stays out of range (p-code tests the full count; x86 `psrad xmm, xmm`).
    /// OR-ing in `out_bits` keeps the result `>= out_bits` at any width.
    fn saturate_shift_count(&mut self, count: ValueId, out_ty: ValueType) -> Result<ValueId> {
        let count_ty = self.builder.value_type(count)?;
        if count_ty.bit_width() <= out_ty.bit_width() {
            return Ok(count);
        }
        let out_bits = u128::try_from(out_ty.bit_width()).expect("bit width fits u128");
        let bound = self.builder.build_int_const(out_bits, count_ty)?;
        let in_range =
            self.builder
                .build_int_cmp_operation(count, bound, IntCmpOp::Less, count_ty)?;
        let too_big = self.build_logical_not(in_range)?;
        let widened = self
            .builder
            .extend_if_needed(too_big, out_ty, ExtendOp::ZeroExtend)?;
        let all_ones = self
            .builder
            .build_int_unary_operation(widened, IntUnaryOp::Neg, out_ty)?;
        let addend =
            self.builder
                .build_const_binop(out_bits, all_ones, IntBinaryOp::And, out_ty)?;
        let narrow = self.builder.truncate_if_needed(count, out_ty)?;
        self.builder
            .build_int_binary_operation(narrow, addend, IntBinaryOp::Or, out_ty)
    }

    /// Operands match the output width, except a shift count, which may be
    /// any width and is saturated then zero-extended.
    pub(super) fn process_int_binary_op(
        &mut self,
        insn: &rsleigh::Insn,
        op: IntBinaryOp,
    ) -> Result<()> {
        let out_vn = require_output_vn(insn)?;
        require_same_size(nth_input_or_err(insn, 0)?, out_vn, "binary op")?;
        if !op.is_shift() {
            require_same_size(nth_input_or_err(insn, 1)?, out_vn, "binary op")?;
        }
        let lhs = self.read_input(insn, 0)?;
        let rhs = self.read_input(insn, 1)?;
        let out_ty = out_vn.int_type()?;
        let rhs = if op.is_shift() {
            let count = self.saturate_shift_count(rhs, out_ty)?;
            self.builder.convert_to_int_if_needed(count, out_ty)?
        } else {
            rhs
        };
        let result = self
            .builder
            .build_int_binary_operation(lhs, rhs, op, out_ty)?;
        self.write_vn(out_vn, result)
    }

    pub(super) fn process_int_cmp_op(&mut self, insn: &rsleigh::Insn, op: IntCmpOp) -> Result<()> {
        self.lift_int_cmp(insn, op, false, false)
    }

    /// Compares two equal-width inputs, optionally swapped and/or negated.
    fn lift_int_cmp(
        &mut self,
        insn: &rsleigh::Insn,
        op: IntCmpOp,
        negate: bool,
        swap: bool,
    ) -> Result<()> {
        let a = nth_input_or_err(insn, 0)?;
        let b = nth_input_or_err(insn, 1)?;
        require_same_size(a, b, "input")?;
        let lhs = self.read_vn(a)?;
        let rhs = self.read_vn(b)?;
        let out_vn = require_output_vn(insn)?;
        let cmp_width = a.int_type()?;
        let (lhs, rhs) = if swap { (rhs, lhs) } else { (lhs, rhs) };
        let cmp = self
            .builder
            .build_int_cmp_operation(lhs, rhs, op, cmp_width)?;
        let result = if negate {
            self.build_logical_not(cmp)?
        } else {
            cmp
        };
        self.write_vn(out_vn, result)
    }

    /// Logical NOT of an `I1`: `Xor(x, IntConst(1)):I1`.
    pub(super) fn build_logical_not(&mut self, x: ValueId) -> Result<ValueId> {
        let one = self.builder.build_boolean_const(true);
        self.builder
            .build_int_binary_operation(x, one, IntBinaryOp::Xor, ValueType::I1)
    }

    pub(super) fn process_bool_binary_op(
        &mut self,
        insn: &rsleigh::Insn,
        op: IntBinaryOp,
    ) -> Result<()> {
        let lhs = self.read_input(insn, 0)?;
        let rhs = self.read_input(insn, 1)?;
        let out_vn = require_output_vn(insn)?;
        let lhs = self.builder.convert_to_int_if_needed(lhs, ValueType::I1)?;
        let rhs = self.builder.convert_to_int_if_needed(rhs, ValueType::I1)?;
        let result = self
            .builder
            .build_int_binary_operation(lhs, rhs, op, ValueType::I1)?;
        self.write_vn(out_vn, result)
    }

    /// `BoolNeg`, lowered to `Xor(x, IntConst(1)):I1`.
    pub(super) fn process_bool_unary_op(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        let value = self.read_input(insn, 0)?;
        let out_vn = require_output_vn(insn)?;
        let value = self
            .builder
            .convert_to_int_if_needed(value, ValueType::I1)?;
        let result = self.build_logical_not(value)?;
        self.write_vn(out_vn, result)
    }

    /// Lowers to `Xor(IntEqual(a, b), IntConst(1)):I1`.
    pub(super) fn handle_int_not_equal(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        self.lift_int_cmp(insn, IntCmpOp::Equal, true, false)
    }

    /// Lowers to `Xor(IntLess(b, a), IntConst(1)):I1`.
    pub(super) fn handle_int_less_equal(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        self.lift_int_cmp(insn, IntCmpOp::Less, true, true)
    }

    /// Lowers to `Xor(IntSless(b, a), IntConst(1)):I1`.
    pub(super) fn handle_int_sless_equal(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        self.lift_int_cmp(insn, IntCmpOp::Sless, true, true)
    }

    /// Lowers `IntSub(a, b)` to `Add(a, Neg(b))`.
    pub(super) fn handle_int_sub(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        let a = nth_input_or_err(insn, 0)?;
        require_same_size(a, nth_input_or_err(insn, 1)?, "input")?;
        let lhs = self.read_vn(a)?;
        let rhs = self.read_input(insn, 1)?;
        let out_vn = require_output_vn(insn)?;
        let out_ty = out_vn.int_type()?;
        require_same_size(a, out_vn, "output")?;
        let neg_rhs = self
            .builder
            .build_int_unary_operation(rhs, IntUnaryOp::Neg, out_ty)?;
        let sum =
            self.builder
                .build_int_binary_operation(lhs, neg_rhs, IntBinaryOp::Add, out_ty)?;
        self.write_vn(out_vn, sum)
    }
}
