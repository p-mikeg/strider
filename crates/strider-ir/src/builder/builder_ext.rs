//! Everything here is pure: no constructor touches lift-time scratch such as
//! the active region or the SSA variable table.

use anyhow::anyhow;

use crate::IRViewer;
use crate::builder::IRBuilder;
use crate::error::Result;
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

    fn truncate_if_needed(&mut self, value_id: ValueId, output_type: ValueType) -> Result<ValueId> {
        let curr_output_type = self.value_type(value_id)?;

        if let Some(val) = self.int_const_u128(value_id) {
            return self.build_int_const(val, output_type);
        }

        if curr_output_type.bit_width() <= output_type.bit_width() {
            return Ok(value_id);
        }

        Ok(self.build_single_output_pure(NodeKind::Truncate, [value_id], output_type))
    }

    /// Only ever widens, or no-ops at equal width. A value wider than
    /// `output_type` is a caller error.
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
            return Err(anyhow!(
                "extend_if_needed: value {value_id:?} ({curr_output_type}) is wider than \
                 target {output_type}; extend cannot narrow — truncate first"
            ));
        }

        if let Some(unsigned_val) = self.int_const_u128(value_id)
            && let Some(signed_val) = self.int_const_i128(value_id)
        {
            // `i128 as u128` reinterprets the sign-extended bits, which
            // `build_int_const` then masks to width.
            return match op {
                ExtendOp::SignExtend => self.build_int_const(signed_val as u128, output_type),
                ExtendOp::ZeroExtend => self.build_int_const(unsigned_val, output_type),
            };
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

    /// There is no `CastToFloat` node: an integer reinterprets bit-for-bit via
    /// `IntBitsToFloat`, and a float of another precision goes through
    /// `FloatToFloat`.
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

    /// Masks `val` to the type's width. Handles any value fitting `u128`,
    /// whatever the declared width.
    fn build_int_const(&mut self, val: impl Into<u128>, output_type: ValueType) -> Result<ValueId> {
        if !output_type.is_integer() {
            return Err(anyhow!(
                "build_int_const called with non-integer type {output_type:?}"
            ));
        }
        let id = self
            .function_mut()
            .intern_int_const(val.into(), output_type);
        Ok(self.build_single_output_pure(NodeKind::IntConst(id), [], output_type))
    }

    /// `limbs` is little-endian. Requires a wide `output_type`.
    fn build_int_const_limbs(&mut self, limbs: &[u64], output_type: ValueType) -> Result<ValueId> {
        if !output_type.is_wide_int() {
            return Err(anyhow!(
                "build_int_const_limbs called with non-wide output type {output_type:?}; \
                 use build_int_const for ≤ I128"
            ));
        }
        let id = self
            .function_mut()
            .intern_int_const_limbs(limbs, output_type);
        Ok(self.build_single_output_pure(NodeKind::IntConst(id), [], output_type))
    }

    /// Strict: both operands must already carry `output_type`.
    fn build_int_binary_operation(
        &mut self,
        lhs_id: ValueId,
        rhs_id: ValueId,
        op: IntBinaryOp,
        output_type: ValueType,
    ) -> Result<ValueId> {
        let lhs_id = self.require_value_type(lhs_id, output_type)?;
        let rhs_id = self.require_value_type(rhs_id, output_type)?;
        Ok(self.build_single_output_pure(NodeKind::IntBinaryOp(op), [lhs_id, rhs_id], output_type))
    }

    /// Builds `x <op> IntConst(k):ty`. Note the operand order: the constant
    /// lands on the right.
    fn build_const_binop(
        &mut self,
        k: u128,
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
        let value = self.require_value_type(input_id, output_type)?;
        Ok(self.build_single_output_pure(NodeKind::IntUnaryOp(op), [value], output_type))
    }

    fn build_popcount(&mut self, input_id: ValueId, output_type: ValueType) -> Result<ValueId> {
        let value = self.require_value_type(input_id, output_type)?;
        Ok(self.build_single_output_pure(NodeKind::Popcount, [value], output_type))
    }

    fn build_lzcount(&mut self, input_id: ValueId, output_type: ValueType) -> Result<ValueId> {
        let value = self.require_value_type(input_id, output_type)?;
        Ok(self.build_single_output_pure(NodeKind::Lzcount, [value], output_type))
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
        let lhs_id = self.require_value_type(lhs_id, operand_type)?;
        let rhs_id = self.require_value_type(rhs_id, operand_type)?;
        Ok(
            self.build_single_output_pure(
                NodeKind::IntCmpOp(kind),
                [lhs_id, rhs_id],
                ValueType::I1,
            ),
        )
    }

    /// `bits` is the raw IEEE 754 pattern. `output_type` must be `F32` or
    /// `F64`.
    fn build_float_const(&mut self, bits: u64, output_type: ValueType) -> ValueId {
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
        let lhs = self.require_value_type(lhs, output_type)?;
        let rhs = self.require_value_type(rhs, output_type)?;
        Ok(self.build_single_output_pure(NodeKind::FloatBinaryOp(op), [lhs, rhs], output_type))
    }

    /// Strict: the operand must already carry `output_type`.
    fn build_float_unary_op(
        &mut self,
        value: ValueId,
        op: FloatUnaryOp,
        output_type: ValueType,
    ) -> Result<ValueId> {
        let coerced = self.require_value_type(value, output_type)?;
        Ok(self.build_single_output_pure(NodeKind::FloatUnaryOp(op), [coerced], output_type))
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
        let rhs = self.require_value_type(rhs, float_ty)?;
        Ok(self.build_single_output_pure(NodeKind::FloatCmpOp(op), [lhs, rhs], ValueType::I1))
    }

    fn build_int_to_float(&mut self, value: ValueId, float_type: ValueType) -> Result<ValueId> {
        self.require_integer_value(value)?;
        Self::require_float_type(float_type)?;
        Ok(self.build_single_output_pure(NodeKind::IntToFloat, [value], float_type))
    }

    fn build_float_to_int(&mut self, value: ValueId, int_type: ValueType) -> Result<ValueId> {
        self.require_float_value(value)?;
        Self::require_integer_type(int_type)?;
        Ok(self.build_single_output_pure(NodeKind::FloatToInt, [value], int_type))
    }

    fn build_float_to_float(&mut self, value: ValueId, float_type: ValueType) -> Result<ValueId> {
        self.require_float_value(value)?;
        Self::require_float_type(float_type)?;
        Ok(self.build_single_output_pure(NodeKind::FloatToFloat, [value], float_type))
    }

    /// Folds an `IntConst` input straight to a `FloatConst` with the same
    /// bits, creating no node.
    fn build_int_bits_to_float(
        &mut self,
        value: ValueId,
        float_type: ValueType,
    ) -> Result<ValueId> {
        self.require_integer_value(value)?;
        Self::require_float_type(float_type)?;
        let input_ty = self.value_type(value)?;
        if input_ty.byte_size() != float_type.byte_size() {
            return Err(anyhow!(
                "IntBitsToFloat width mismatch: input {input_ty:?} ({} bytes) \
                 vs float {float_type:?} ({} bytes)",
                input_ty.byte_size(),
                float_type.byte_size(),
            ));
        }
        // F80 skips the fold: `FloatConst`'s u64 payload cannot hold an
        // 80-bit pattern.
        if let Some(bits) = self.int_const_u128(value)
            && float_type != ValueType::F80
        {
            // Already masked to the type's width, and F32/F64 fit a u64.
            #[allow(clippy::cast_possible_truncation)]
            return Ok(self.build_float_const(bits as u64, float_type));
        }
        Ok(self.build_single_output_pure(NodeKind::IntBitsToFloat, [value], float_type))
    }

    /// Folds a `FloatConst` input straight to an `IntConst` with the same
    /// bits, creating no node.
    fn build_float_bits_to_int(&mut self, value: ValueId, int_type: ValueType) -> Result<ValueId> {
        self.require_float_value(value)?;
        Self::require_integer_type(int_type)?;
        let input_ty = self.value_type(value)?;
        if input_ty.byte_size() != int_type.byte_size() {
            return Err(anyhow!(
                "FloatBitsToInt width mismatch: input {input_ty:?} ({} bytes) \
                 vs int {int_type:?} ({} bytes)",
                input_ty.byte_size(),
                int_type.byte_size(),
            ));
        }
        // F80 skips the fold: a u64 payload cannot represent the whole 80-bit
        // pattern.
        if let NodeKind::FloatConst(bits) = *self.function().kind_of_value(value)
            && input_ty != ValueType::F80
        {
            return self.build_int_const(bits, int_type);
        }
        Ok(self.build_single_output_pure(NodeKind::FloatBitsToInt, [value], int_type))
    }

    fn build_segment_op(
        &mut self,
        op_id: u64,
        segment: ValueId,
        offset: ValueId,
        output_type: ValueType,
    ) -> Result<ValueId> {
        self.validate_value_inputs(&[segment, offset])?;
        Ok(self.build_single_output_pure(
            NodeKind::SegmentOp { op_id },
            [segment, offset],
            output_type,
        ))
    }

    fn build_opaque_variadic(
        &mut self,
        kind: NodeKind,
        inputs: &[ValueId],
        output_type: ValueType,
    ) -> Result<ValueId> {
        self.validate_value_inputs(inputs)?;
        let node = self.create_node(
            kind,
            inputs.iter().copied(),
            [ValueKind::Typed(output_type)],
        );
        let [value] = self.function().node_outputs_exact(node)?;
        Ok(value)
    }

    fn build_cpool_ref(&mut self, refs: &[ValueId], output_type: ValueType) -> Result<ValueId> {
        self.build_opaque_variadic(NodeKind::CPoolRef, refs, output_type)
    }

    fn build_new(&mut self, args: &[ValueId], output_type: ValueType) -> Result<ValueId> {
        self.build_opaque_variadic(NodeKind::New, args, output_type)
    }
}

impl<B: IRBuilder + ?Sized> IRBuilderExt for B {}
