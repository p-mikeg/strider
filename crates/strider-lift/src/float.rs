use strider_ir::node::{ValueId, ValueType};
use strider_ir::{
    FloatBinaryOp, FloatCmpOp, FloatUnaryOp, FunctionBuilder, IRBuilderExt, IRViewer, IntBinaryOp,
    VnTypeExt,
};

use anyhow::Result;

use crate::FunctionLifter;
use crate::pcode_util::{nth_input_or_err, require_output_vn};

impl<'a, R: rsleigh::MemReader> FunctionLifter<'a, R> {
    /// Registers hold float results as integers, so bitcast on the way out.
    pub(super) fn write_float_to_vn(&mut self, vn: &rsleigh::Vn, float_val: ValueId) -> Result<()> {
        let int_ty = vn.int_type()?;
        let int_val = self.builder.build_float_bits_to_int(float_val, int_ty)?;
        self.write_vn(vn, int_val)
    }

    /// Two-input float op over operands bitcast to the output's float type.
    fn lift_float_binary(
        &mut self,
        insn: &rsleigh::Insn,
        build: impl FnOnce(&mut FunctionBuilder, ValueId, ValueId, ValueType) -> Result<ValueId>,
    ) -> Result<()> {
        let lhs = self.read_input(insn, 0)?;
        let rhs = self.read_input(insn, 1)?;
        let out_vn = require_output_vn(insn)?;
        let float_ty = out_vn.float_type()?;
        let lhs = self.builder.cast_to_float_if_needed(lhs, float_ty)?;
        let rhs = self.builder.cast_to_float_if_needed(rhs, float_ty)?;
        let result = build(&mut self.builder, lhs, rhs, float_ty)?;
        self.write_float_to_vn(out_vn, result)
    }

    fn lift_float_unary(
        &mut self,
        insn: &rsleigh::Insn,
        build: impl FnOnce(&mut FunctionBuilder, ValueId, ValueType) -> Result<ValueId>,
    ) -> Result<()> {
        let value = self.read_input(insn, 0)?;
        let out_vn = require_output_vn(insn)?;
        let float_ty = out_vn.float_type()?;
        let value = self.builder.cast_to_float_if_needed(value, float_ty)?;
        let result = build(&mut self.builder, value, float_ty)?;
        self.write_float_to_vn(out_vn, result)
    }

    pub(super) fn process_float_binary_op(
        &mut self,
        insn: &rsleigh::Insn,
        op: FloatBinaryOp,
    ) -> Result<()> {
        self.lift_float_binary(insn, |b, l, r, t| b.build_float_binary_op(l, r, op, t))
    }

    pub(super) fn process_float_unary_op(
        &mut self,
        insn: &rsleigh::Insn,
        op: FloatUnaryOp,
    ) -> Result<()> {
        self.lift_float_unary(insn, |b, v, t| b.build_float_unary_op(v, op, t))
    }

    pub(super) fn process_float_cmp_op(
        &mut self,
        insn: &rsleigh::Insn,
        op: FloatCmpOp,
    ) -> Result<()> {
        let lhs = self.read_input(insn, 0)?;
        let rhs = self.read_input(insn, 1)?;
        let out_vn = require_output_vn(insn)?;
        let (lhs, rhs) = self.cast_float_cmp_operands(lhs, rhs)?;
        let result = self.builder.build_float_cmp_op(lhs, rhs, op)?;
        self.write_vn(out_vn, result)
    }

    /// Common float type is inferred from `lhs`.
    fn cast_float_cmp_operands(
        &mut self,
        lhs: ValueId,
        rhs: ValueId,
    ) -> Result<(ValueId, ValueId)> {
        let float_ty = self.builder.infer_float_type(lhs)?;
        let lhs = self.builder.cast_to_float_if_needed(lhs, float_ty)?;
        let rhs = self.builder.cast_to_float_if_needed(rhs, float_ty)?;
        Ok((lhs, rhs))
    }

    /// `Xor(FloatEqual(lhs, rhs), IntConst(1)):I1`: true on a NaN operand.
    fn build_float_eq_negated(&mut self, lhs: ValueId, rhs: ValueId) -> Result<ValueId> {
        let (lhs, rhs) = self.cast_float_cmp_operands(lhs, rhs)?;
        let eq = self
            .builder
            .build_float_cmp_op(lhs, rhs, FloatCmpOp::Equal)?;
        self.build_logical_not(eq)
    }

    pub(super) fn handle_float_nan(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        let value = self.read_input(insn, 0)?;
        let out_vn = require_output_vn(insn)?;
        // `is_nan(x)` is `x != x`.
        let result = self.build_float_eq_negated(value, value)?;
        self.write_vn(out_vn, result)
    }

    /// Lowers `FloatSub(a, b)` to `FloatAdd(a, Neg(b))`. Exact except a NaN
    /// `b`'s sign, which flips (`1.0 - qNaN` gives `-qNaN`, hardware `+qNaN`);
    /// IEEE 754 leaves it unspecified.
    pub(super) fn handle_float_sub(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        self.lift_float_binary(insn, |b, l, r, t| {
            let neg_r = b.build_float_unary_op(r, FloatUnaryOp::Neg, t)?;
            b.build_float_binary_op(l, neg_r, FloatBinaryOp::Add, t)
        })
    }

    /// Lowers to `Xor(FloatEqual(a, b), IntConst(1)):I1`.
    pub(super) fn handle_float_not_equal(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        let lhs = self.read_input(insn, 0)?;
        let rhs = self.read_input(insn, 1)?;
        let out_vn = require_output_vn(insn)?;
        let result = self.build_float_eq_negated(lhs, rhs)?;
        self.write_vn(out_vn, result)
    }

    /// Lowers to `Or(FloatLess(a, b), FloatEqual(a, b))`, false on NaN; the
    /// integer path's `Not(Less(b, a))` would be true.
    pub(super) fn handle_float_less_equal(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        let lhs = self.read_input(insn, 0)?;
        let rhs = self.read_input(insn, 1)?;
        let out_vn = require_output_vn(insn)?;
        let (lhs, rhs) = self.cast_float_cmp_operands(lhs, rhs)?;
        let lt = self
            .builder
            .build_float_cmp_op(lhs, rhs, FloatCmpOp::Less)?;
        let eq = self
            .builder
            .build_float_cmp_op(lhs, rhs, FloatCmpOp::Equal)?;
        let result =
            self.builder
                .build_int_binary_operation(lt, eq, IntBinaryOp::Or, ValueType::I1)?;
        self.write_vn(out_vn, result)
    }

    pub(super) fn handle_float_int_to_float(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        let raw_value = self.read_input(insn, 0)?;
        let out_vn = require_output_vn(insn)?;
        let float_ty = out_vn.float_type()?;
        // Reads are int-typed; this coerces the width.
        let in_size = nth_input_or_err(insn, 0)?.int_type()?;
        let int_value = self.builder.convert_to_int_if_needed(raw_value, in_size)?;
        let float_result = self.builder.build_int_to_float(int_value, float_ty)?;
        self.write_float_to_vn(out_vn, float_result)
    }

    pub(super) fn handle_float_float_to_float(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        let raw_value = self.read_input(insn, 0)?;
        let out_vn = require_output_vn(insn)?;
        let out_float_ty = out_vn.float_type()?;
        // Reads are int-typed; bitcast at the input's own float width.
        let in_float_ty = nth_input_or_err(insn, 0)?.float_type()?;
        let float_value = self
            .builder
            .cast_to_float_if_needed(raw_value, in_float_ty)?;
        let float_result = self
            .builder
            .build_float_to_float(float_value, out_float_ty)?;
        self.write_float_to_vn(out_vn, float_result)
    }

    pub(super) fn handle_float_trunc(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        let raw_value = self.read_input(insn, 0)?;
        let out_vn = require_output_vn(insn)?;
        let int_ty = out_vn.int_type()?;
        let in_float_ty = nth_input_or_err(insn, 0)?.float_type()?;
        let float_value = self
            .builder
            .cast_to_float_if_needed(raw_value, in_float_ty)?;
        let int_result = self.builder.build_float_to_int(float_value, int_ty)?;
        self.write_vn(out_vn, int_result)
    }
}
