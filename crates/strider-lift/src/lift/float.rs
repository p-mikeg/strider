use strider_ir::node::ValueType;
use strider_ir::{
    FloatBinaryOp, FloatCmpOp, FloatUnaryOp, IRBuilderExt, IRViewer, IntBinaryOp, VnTypeExt,
};

use crate::lift::FunctionLifter;
use crate::lift::pcode_util::{Result, nth_input_or_err, require_output_vn};

impl<'a, R: rsleigh::MemReader> FunctionLifter<'a, R> {
    /// Registers hold float results as integers, so bitcast on the way out.
    pub(super) fn write_float_to_vn(
        &mut self,
        vn: &rsleigh::Vn,
        float_val: strider_ir::Value,
    ) -> Result<()> {
        let int_ty: ValueType = vn.int_type()?;
        let int_val = self.builder.build_float_bits_to_int(float_val, int_ty)?;
        self.write_vn(vn, int_val)
    }

    /// Shared envelope for two-input float ops.  The operand casts insert the
    /// `IntBitsToFloat` bitcast the strict builders require.
    fn lift_float_binary(
        &mut self,
        insn: &rsleigh::Insn,
        build: impl FnOnce(
            &mut strider_ir::FunctionBuilder,
            strider_ir::Value,
            strider_ir::Value,
            ValueType,
        ) -> Result<strider_ir::Value>,
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
        build: impl FnOnce(
            &mut strider_ir::FunctionBuilder,
            strider_ir::Value,
            ValueType,
        ) -> Result<strider_ir::Value>,
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
        lhs: strider_ir::Value,
        rhs: strider_ir::Value,
    ) -> Result<(strider_ir::Value, strider_ir::Value)> {
        let float_ty = self.builder.infer_float_type(lhs)?;
        let lhs = self.builder.cast_to_float_if_needed(lhs, float_ty)?;
        let rhs = self.builder.cast_to_float_if_needed(rhs, float_ty)?;
        Ok((lhs, rhs))
    }

    /// `Xor(FloatEqual(lhs, rhs), IntConst(1)):I1`, shared by `FloatNan` and
    /// `FloatNotEqual`.  Sound under IEEE 754: `Equal` is false when either
    /// operand is NaN, so the negation is true, matching `NotEqual`/`is_nan`.
    fn build_float_eq_negated(
        &mut self,
        lhs: strider_ir::Value,
        rhs: strider_ir::Value,
    ) -> Result<strider_ir::Value> {
        let (lhs, rhs) = self.cast_float_cmp_operands(lhs, rhs)?;
        let eq = self
            .builder
            .build_float_cmp_op(lhs, rhs, FloatCmpOp::Equal)?;
        self.build_logical_not(eq)
    }

    pub(super) fn handle_float_nan(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        let value = self.read_input(insn, 0)?;
        let out_vn = require_output_vn(insn)?;
        // `is_nan(x)` is `x != x` under IEEE 754, built directly in the
        // lowered form since there is no `FloatCmpOp::NotEqual`.
        let result = self.build_float_eq_negated(value, value)?;
        self.write_vn(out_vn, result)
    }

    /// Lowers `FloatSub(a, b)` to `FloatAdd(a, Neg(b))`.  Exact under IEEE 754:
    /// `a - b` equals `a + (-b)` for finite values, and negation flips the sign
    /// bit of infinities and NaNs without changing NaN-ness.
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

    /// Lowers to `Or(FloatLess(a, b), FloatEqual(a, b))`, NOT to the swapped
    /// negated form the integer path uses: `Not(Less(b, a))` returns TRUE on a
    /// NaN operand, where IEEE 754 requires false.  Both `Less` and `Equal`
    /// return false on NaN, so their `Or` is correctly false.
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
        // The input is int-typed (register reads always are), so this coerces
        // its width. A non-int (float-typed UNIQUE temp) input errors here
        // rather than being silently reinterpreted.
        let in_size: ValueType = nth_input_or_err(insn, 0)?.int_type()?;
        let int_value = self.builder.convert_to_int_if_needed(raw_value, in_size)?;
        let float_result = self.builder.build_int_to_float(int_value, float_ty)?;
        self.write_float_to_vn(out_vn, float_result)
    }

    pub(super) fn handle_float_float_to_float(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        let raw_value = self.read_input(insn, 0)?;
        let out_vn = require_output_vn(insn)?;
        let out_float_ty = out_vn.float_type()?;
        // Register reads are int-typed, so cast via the input's natural float
        // width (4 bytes to F32, 8 to F64) before the strict builder.
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
        let int_ty: ValueType = out_vn.int_type()?;
        let in_float_ty = nth_input_or_err(insn, 0)?.float_type()?;
        let float_value = self
            .builder
            .cast_to_float_if_needed(raw_value, in_float_ty)?;
        let int_result = self.builder.build_float_to_int(float_value, int_ty)?;
        self.write_vn(out_vn, int_result)
    }
}
