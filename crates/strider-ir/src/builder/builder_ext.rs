//! Everything here is pure: no constructor touches lift-time scratch such as
//! the active region or the SSA variable table.

use anyhow::anyhow;
use dashu_int::UBig;

use crate::IRViewer;
use crate::builder::IRBuilder;
use crate::builder::checks::{
    require_float_type, require_float_value, require_integer_type, require_integer_value,
    require_output_type, require_slot, require_value_type,
};
use crate::error::Result;
use crate::eval;
use crate::node::{
    ExtendOp, FloatBinaryOp, FloatCmpOp, FloatUnaryOp, IntBinaryOp, IntCmpOp, IntUnaryOp, NodeKind,
    ValueId, ValueKind, ValueType,
};

pub trait IRBuilderExt: IRBuilder {
    fn build_single_output_pure(
        &mut self,
        kind: NodeKind,
        inputs: impl IntoIterator<Item = ValueId>,
        output_type: ValueType,
    ) -> ValueId {
        let node = self.create_node(kind, inputs, [ValueKind::Typed(output_type)]);
        self.function().node_outputs(node)[0]
    }

    /// Narrows a wider value; no-ops at equal-or-narrower width, whether or not
    /// the value is constant.
    fn truncate_if_needed(&mut self, value_id: ValueId, output_type: ValueType) -> Result<ValueId> {
        let curr_output_type = self.value_type(value_id)?;

        if curr_output_type.bit_width() <= output_type.bit_width() {
            return Ok(value_id);
        }

        // A constant narrows by re-interning at the target width, which masks;
        // `Truncate` would take the same low bits through a node.
        if let Some(val) = self.int_const_u128(value_id) {
            return self.build_int_const(val, output_type);
        }

        Ok(self.build_single_output_pure(NodeKind::Truncate, [value_id], output_type))
    }

    /// Widens a narrower value; no-ops at equal-or-wider width.
    fn extend_if_needed(
        &mut self,
        value_id: ValueId,
        output_type: ValueType,
        op: ExtendOp,
    ) -> Result<ValueId> {
        let curr_output_type = self.value_type(value_id)?;

        if !output_type.is_integer() {
            return Err(anyhow!(
                "output {value_id:?} target is not an integer value"
            ));
        }
        if !curr_output_type.is_integer() {
            return Err(anyhow!(
                "cannot integer-extend non-integer value {value_id:?} \
                 ({curr_output_type}); a bitcast is required first"
            ));
        }
        if curr_output_type.bit_width() > output_type.bit_width() {
            return Ok(value_id);
        }

        // Both types are integers by the checks above, so `eval::mask` is the
        // width reduction and not a float's empty bit set.
        if let Some(v) = self.int_const(value_id).cloned() {
            let folded = match op {
                ExtendOp::SignExtend => eval::sign_extend(&v, curr_output_type, output_type),
                ExtendOp::ZeroExtend => Some(eval::mask(&v, output_type)),
            };
            if let Some(k) = folded {
                return self.build_int_const(k, output_type);
            }
        }

        if curr_output_type.bit_width() == output_type.bit_width() {
            return Ok(value_id);
        }
        Ok(self.build_single_output_pure(NodeKind::Extend(op), [value_id], output_type))
    }

    /// Keys on bit width, not byte size, so an `I1` still zero-extends up to
    /// `I8` despite the two sharing a byte.
    fn convert_to_int_if_needed(
        &mut self,
        value_id: ValueId,
        output_type: ValueType,
    ) -> Result<ValueId> {
        let curr_output_type = self.value_type(value_id)?;
        if !curr_output_type.is_integer() {
            return Err(anyhow!(
                "cannot convert non-integer value {value_id:?} \
                 ({curr_output_type}) to an integer; a bitcast is required first"
            ));
        }
        let truncate_id = self.truncate_if_needed(value_id, output_type)?;
        self.extend_if_needed(truncate_id, output_type, ExtendOp::ZeroExtend)
    }

    /// An integer input reinterprets bit-for-bit; another float precision
    /// changes precision.
    fn cast_to_float_if_needed(&mut self, input: ValueId, float_ty: ValueType) -> Result<ValueId> {
        let in_ty = self.value_type(input)?;
        if in_ty == float_ty {
            return Ok(input);
        }
        if in_ty.is_float() {
            self.build_float_to_float(input, float_ty)
        } else {
            self.build_int_bits_to_float(input, float_ty)
        }
    }

    /// An `IntConst` at `I1`.
    fn build_boolean_const(&mut self, val: bool) -> ValueId {
        // I1 is an integer, so the guard inside can never fire.
        self.build_int_const(u128::from(val), ValueType::I1)
            .expect("I1 is always an integer type")
    }

    /// Masks `val` to the type's width, at any width: `Into<UBig>` takes
    /// every unsigned primitive and a `UBig` itself.
    fn build_int_const(&mut self, val: impl Into<UBig>, output_type: ValueType) -> Result<ValueId> {
        if !output_type.is_integer() {
            return Err(anyhow!(
                "build_int_const called with non-integer type {output_type:?}"
            ));
        }
        let id = self.intern_int_const(val, output_type);
        Ok(self.build_single_output_pure(NodeKind::IntConst(id), [], output_type))
    }

    /// The `IntConst` of `ty` with the window `[shift, shift + window_bits)`
    /// cleared and every other bit set: ANDing a container with it preserves
    /// exactly the bits the window does not cover.
    ///
    /// The window is clipped by `ty`, not by its own arithmetic: whatever of
    /// it lies above the width is simply not part of the value.
    fn build_window_preserve_mask(
        &mut self,
        window_bits: u64,
        shift: u64,
        ty: ValueType,
    ) -> Result<ValueId> {
        let window = eval::ones_at(window_bits as usize, shift as usize);
        self.build_int_const(eval::complement(&window, ty), ty)
    }

    /// The all-ones `IntConst` of `ty`, at any integer width.
    fn build_all_ones(&mut self, ty: ValueType) -> Result<ValueId> {
        self.build_int_const(eval::all_ones(ty), ty)
    }

    /// Strict: both operands must already carry `output_type`, a shift's count
    /// included. p-code leaves that count any width and `validate` exempts it,
    /// so a caller lowering a shift coerces the count first.
    fn build_int_binary_operation(
        &mut self,
        lhs_id: ValueId,
        rhs_id: ValueId,
        op: IntBinaryOp,
        output_type: ValueType,
    ) -> Result<ValueId> {
        let kind = NodeKind::IntBinaryOp(op);
        require_output_type(&kind, output_type)?;
        let lhs_id = require_value_type(self.function(), lhs_id, output_type)?;
        let rhs_id = require_value_type(self.function(), rhs_id, output_type)?;
        Ok(self.build_single_output_pure(kind, [lhs_id, rhs_id], output_type))
    }

    /// Builds `x <op> IntConst(k):ty`; the constant lands on the right.
    fn build_const_binop(
        &mut self,
        k: impl Into<UBig>,
        x: ValueId,
        op: IntBinaryOp,
        ty: ValueType,
    ) -> Result<ValueId> {
        let kc = self.build_int_const(k, ty)?;
        self.build_int_binary_operation(x, kc, op, ty)
    }

    /// Returns `value` untouched on a zero shift.
    fn build_shift_by_const(
        &mut self,
        value: ValueId,
        shift_bits: u64,
        op: IntBinaryOp,
        ty: ValueType,
    ) -> Result<ValueId> {
        if shift_bits == 0 {
            return Ok(value);
        }
        self.build_const_binop(u128::from(shift_bits), value, op, ty)
    }

    /// Strict: the operand must already carry `output_type`.
    fn build_int_unary_operation(
        &mut self,
        input_id: ValueId,
        op: IntUnaryOp,
        output_type: ValueType,
    ) -> Result<ValueId> {
        let kind = NodeKind::IntUnaryOp(op);
        require_output_type(&kind, output_type)?;
        let value = require_value_type(self.function(), input_id, output_type)?;
        Ok(self.build_single_output_pure(kind, [value], output_type))
    }

    /// Counts over the INPUT's width; `output_type` only sizes the result, so
    /// the two are independent. Coercing the input would shift the count by the
    /// width difference.
    fn build_popcount(&mut self, input_id: ValueId, output_type: ValueType) -> Result<ValueId> {
        require_integer_type(self.value_type(input_id)?)?;
        require_integer_type(output_type)?;
        Ok(self.build_single_output_pure(NodeKind::Popcount, [input_id], output_type))
    }

    /// Counts leading zeros over the INPUT's width; see [`Self::build_popcount`].
    fn build_lzcount(&mut self, input_id: ValueId, output_type: ValueType) -> Result<ValueId> {
        require_integer_type(self.value_type(input_id)?)?;
        require_integer_type(output_type)?;
        Ok(self.build_single_output_pure(NodeKind::Lzcount, [input_id], output_type))
    }

    /// Outputs `I1`. Strict: both operands must already carry `operand_type`,
    /// the comparison width.
    fn build_int_cmp_operation(
        &mut self,
        lhs_id: ValueId,
        rhs_id: ValueId,
        kind: IntCmpOp,
        operand_type: ValueType,
    ) -> Result<ValueId> {
        let kind = NodeKind::IntCmpOp(kind);
        let lhs_id = require_value_type(self.function(), lhs_id, operand_type)?;
        let rhs_id = require_value_type(self.function(), rhs_id, operand_type)?;
        require_slot(self.function(), &kind, 0, lhs_id)?;
        require_slot(self.function(), &kind, 1, rhs_id)?;
        Ok(self.build_single_output_pure(kind, [lhs_id, rhs_id], ValueType::I1))
    }

    /// `bits` is the raw IEEE 754 pattern, masked to `output_type`'s width so
    /// two patterns differing only above it dedup onto one node.
    ///
    /// # Panics
    ///
    /// On a non-float `output_type`, or one wider than `FloatConst`'s `u64`
    /// payload (`F80` / `F128`), which would keep only the low 8 bytes.
    fn build_float_const(&mut self, bits: u64, output_type: ValueType) -> ValueId {
        assert!(
            output_type.is_float() && output_type.byte_size() <= 8,
            "build_float_const needs a float type of at most 8 bytes; got {output_type:?}"
        );
        let bits = output_type.mask_float_bits(bits);
        self.build_single_output_pure(NodeKind::FloatConst(bits), [], output_type)
    }

    /// Strict: both operands must already carry the float `output_type`.
    fn build_float_binary_op(
        &mut self,
        lhs: ValueId,
        rhs: ValueId,
        op: FloatBinaryOp,
        output_type: ValueType,
    ) -> Result<ValueId> {
        let kind = NodeKind::FloatBinaryOp(op);
        require_output_type(&kind, output_type)?;
        let lhs = require_value_type(self.function(), lhs, output_type)?;
        let rhs = require_value_type(self.function(), rhs, output_type)?;
        Ok(self.build_single_output_pure(kind, [lhs, rhs], output_type))
    }

    /// Strict: the operand must already carry `output_type`.
    fn build_float_unary_op(
        &mut self,
        value: ValueId,
        op: FloatUnaryOp,
        output_type: ValueType,
    ) -> Result<ValueId> {
        let kind = NodeKind::FloatUnaryOp(op);
        require_output_type(&kind, output_type)?;
        let coerced = require_value_type(self.function(), value, output_type)?;
        Ok(self.build_single_output_pure(kind, [coerced], output_type))
    }

    /// Outputs `I1`. Strict: both operands must already share one float type.
    fn build_float_cmp_op(
        &mut self,
        lhs: ValueId,
        rhs: ValueId,
        op: FloatCmpOp,
    ) -> Result<ValueId> {
        let float_ty = self.value_type(lhs)?;
        if !float_ty.is_float() {
            return Err(anyhow!(
                "build_float_cmp_op: lhs {lhs:?} has type {float_ty}, expected a float"
            ));
        }
        let rhs = require_value_type(self.function(), rhs, float_ty)?;
        Ok(self.build_single_output_pure(NodeKind::FloatCmpOp(op), [lhs, rhs], ValueType::I1))
    }

    fn build_int_to_float(&mut self, value: ValueId, float_type: ValueType) -> Result<ValueId> {
        require_integer_value(self.function(), value)?;
        require_float_type(float_type)?;
        Ok(self.build_single_output_pure(NodeKind::IntToFloat, [value], float_type))
    }

    fn build_float_to_int(&mut self, value: ValueId, int_type: ValueType) -> Result<ValueId> {
        require_float_value(self.function(), value)?;
        require_integer_type(int_type)?;
        Ok(self.build_single_output_pure(NodeKind::FloatToInt, [value], int_type))
    }

    fn build_float_to_float(&mut self, value: ValueId, float_type: ValueType) -> Result<ValueId> {
        require_float_value(self.function(), value)?;
        require_float_type(float_type)?;
        Ok(self.build_single_output_pure(NodeKind::FloatToFloat, [value], float_type))
    }

    /// An `IntConst` input up to 8 bytes folds to a `FloatConst` with the same
    /// bits. Anything else builds the conversion node, `F80` and `F128`
    /// included: `FloatConst` carries a `u64`.
    fn build_int_bits_to_float(
        &mut self,
        value: ValueId,
        float_type: ValueType,
    ) -> Result<ValueId> {
        require_integer_value(self.function(), value)?;
        require_float_type(float_type)?;
        let input_ty = self.value_type(value)?;
        if input_ty.byte_size() != float_type.byte_size() {
            return Err(anyhow!(
                "IntBitsToFloat width mismatch: input {input_ty:?} ({} bytes) \
                 vs float {float_type:?} ({} bytes)",
                input_ty.byte_size(),
                float_type.byte_size(),
            ));
        }
        // `FloatConst`'s payload is a u64, so a wider pattern cannot fold.
        if let Some(bits) = self.int_const_u128(value)
            && float_type.byte_size() <= 8
        {
            // Already masked to the type's width.
            return Ok(self.build_float_const(bits as u64, float_type));
        }
        Ok(self.build_single_output_pure(NodeKind::IntBitsToFloat, [value], float_type))
    }

    /// A `FloatConst` input up to 8 bytes folds to an `IntConst` with the same
    /// bits. Anything else builds the conversion node.
    fn build_float_bits_to_int(&mut self, value: ValueId, int_type: ValueType) -> Result<ValueId> {
        require_float_value(self.function(), value)?;
        require_integer_type(int_type)?;
        let input_ty = self.value_type(value)?;
        if input_ty.byte_size() != int_type.byte_size() {
            return Err(anyhow!(
                "FloatBitsToInt width mismatch: input {input_ty:?} ({} bytes) \
                 vs int {int_type:?} ({} bytes)",
                input_ty.byte_size(),
                int_type.byte_size(),
            ));
        }
        // A u64 payload cannot represent a wider pattern.
        if let NodeKind::FloatConst(bits) = *self.function().kind_of_value(value)
            && input_ty.byte_size() <= 8
        {
            return self.build_int_const(bits, int_type);
        }
        Ok(self.build_single_output_pure(NodeKind::FloatBitsToInt, [value], int_type))
    }

    fn build_opaque_variadic(
        &mut self,
        kind: NodeKind,
        inputs: &[ValueId],
        output_type: ValueType,
    ) -> Result<ValueId> {
        for (slot, &value) in inputs.iter().enumerate() {
            require_slot(self.function(), &kind, slot, value)?;
        }
        require_output_type(&kind, output_type)?;
        Ok(self.build_single_output_pure(kind, inputs.iter().copied(), output_type))
    }

    fn build_cpool_ref(&mut self, refs: &[ValueId], output_type: ValueType) -> Result<ValueId> {
        self.build_opaque_variadic(NodeKind::CPoolRef, refs, output_type)
    }

    fn build_new(&mut self, args: &[ValueId], output_type: ValueType) -> Result<ValueId> {
        self.build_opaque_variadic(NodeKind::New, args, output_type)
    }
}

impl<B: IRBuilder + ?Sized> IRBuilderExt for B {}
