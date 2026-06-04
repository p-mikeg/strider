use anyhow::anyhow;

use super::FunctionBuilder;
use crate::builder::IRBuilderExt;
use crate::error::Result;
use crate::node::{NodeKind, ValueId, ValueType};
use crate::ops::ExtendOp;

/// Unified return shape for [`FunctionBuilder::const_value`].
///
/// `Int { val, ty }` carries the raw `u128` payload of an `IntConst`
/// node alongside its declared `ValueType` so callers can decide
/// whether to view it unsigned / signed / mask / etc.  `Float` carries
/// the raw bit pattern of a `FloatConst` — the analyzer never needs
/// the float type for constant folding (`f32` vs `f64` is inferred
/// from the surrounding op), so the type isn't carried here.
#[derive(Debug, Clone, Copy)]
pub(crate) enum ConstValue {
    Int { val: u128, ty: ValueType },
    Float { bits: u64 },
}

impl FunctionBuilder {
    /// Retrieves the [`ValueType`] of `value_id`.
    ///
    /// Returns an error if the output does not carry a value (e.g. it is a
    /// control or memory edge).
    ///
    /// # Errors
    ///
    /// Returns `ExpectedValue` when `value_id` is a control,
    /// memory, or control-phi edge.
    pub fn value_type(&self, value_id: ValueId) -> Result<ValueType> {
        let kind = self.function().value_kind(value_id);
        kind.as_value()
            .ok_or_else(|| anyhow!("output {value_id:?} is not a value edge (got {kind:?})"))
    }

    /// Asserts that `value_id` already carries exactly `expected`, returning
    /// it unchanged on success.  This is the strict counterpart to the
    /// coercion helpers: the value-producing `build_*` constructors call it
    /// instead of silently truncating / extending / bit-casting an operand,
    /// so the **lifter** (or any caller) is responsible for inserting the
    /// right fix-up node beforehand.  A mismatch is a hard error rather than
    /// wrong IR.
    ///
    /// # Errors
    ///
    /// Returns an error when `value_id` is not a value edge, or when its
    /// type differs from `expected`.
    pub fn require_value_type(
        &self,
        value_id: ValueId,
        expected: ValueType,
    ) -> Result<ValueId> {
        let actual = self.value_type(value_id)?;
        if actual != expected {
            return Err(anyhow!(
                "operand {value_id:?} has type {actual} but the operation \
                 requires {expected}; the caller must insert the truncate / \
                 extend / bitcast fix-up (builders no longer auto-coerce)"
            ));
        }
        Ok(value_id)
    }

    /// Returns the constant value carried by `value_id` if its defining
    /// node is `IntConst` or `FloatConst`; `Ok(None)` otherwise.  The
    /// `get_as_*` helpers below are thin projections off this unified
    /// shape.  Booleans are `IntConst` values typed `I1`.
    ///
    /// # Errors
    ///
    /// Returns `ExpectedValue` when `value_id` is not a value edge.
    pub(crate) fn const_value(&self, value_id: ValueId) -> Result<Option<ConstValue>> {
        let ty = self.value_type(value_id)?;
        Ok(match self.function().kind_of_value(value_id) {
            NodeKind::IntConst(val) if ty.is_integer() => Some(ConstValue::Int { val: *val, ty }),
            NodeKind::FloatConst(bits) if ty.is_float() => {
                Some(ConstValue::Float { bits: *bits })
            }
            _ => None,
        })
    }

    /// If `value_id` is a constant node, returns its value truncated to the
    /// declared [`ValueType`] as an unsigned 64-bit integer.
    ///
    /// Returns `Ok(None)` for non-constant nodes.
    ///
    /// # Errors
    ///
    /// Returns `ExpectedValue` when `value_id` is not a value
    /// edge.
    pub fn get_as_unsigned_int(&self, value_id: ValueId) -> Result<Option<u64>> {
        Ok(self.const_value(value_id)?.and_then(|c| match c {
            ConstValue::Int { val, ty } => {
                ty.get_unsigned_int(val).and_then(|v| u64::try_from(v).ok())
            }
            ConstValue::Float { .. } => None,
        }))
    }

    /// If `value_id` is an integer constant, returns its value
    /// sign-extended to `i64` according to the declared [`ValueType`].
    /// An `I1` boolean folds as `0` / `1` per [`Self::get_as_unsigned_int`].
    ///
    /// Returns `Ok(None)` for non-constant nodes.
    ///
    /// # Errors
    ///
    /// Returns `ExpectedValue` when `value_id` is not a value
    /// edge.
    pub fn get_as_signed_int(&self, value_id: ValueId) -> Result<Option<i64>> {
        Ok(self.const_value(value_id)?.and_then(|c| match c {
            ConstValue::Int { val, ty } => {
                ty.get_signed_int(val).and_then(|v| i64::try_from(v).ok())
            }
            ConstValue::Float { .. } => None,
        }))
    }

    /// Returns both the unsigned and signed interpretations of `value_id` if
    /// it is an integer constant, or `None` otherwise.
    ///
    /// # Errors
    ///
    /// Returns `ExpectedValue` when `value_id` is not a value
    /// edge.
    pub fn get_as_int(&self, value_id: ValueId) -> Result<Option<(u64, i64)>> {
        Ok(self.get_as_unsigned_int(value_id)?.zip(self.get_as_signed_int(value_id)?))
    }

    /// If `value_id` is a `FloatConst` node, returns its raw bit pattern.
    /// Returns `Ok(None)` for non-constant nodes.
    ///
    /// # Errors
    ///
    /// Returns `ExpectedValue` when `value_id` is not a value
    /// edge.
    pub fn get_as_float_bits(&self, value_id: ValueId) -> Result<Option<u64>> {
        Ok(self.const_value(value_id)?.and_then(|c| match c {
            ConstValue::Float { bits } => Some(bits),
            _ => None,
        }))
    }

    /// Truncates `value_id` to `output_type` if it is currently wider.
    ///
    /// # Errors
    ///
    /// Returns `ExpectedValue` when `value_id` is not a value
    /// edge.
    pub fn truncate_if_needed(
        &mut self,
        value_id: ValueId,
        output_type: ValueType,
    ) -> Result<ValueId> {
        let curr_output_type = self.value_type(value_id)?;

        if let Some(val) = self.get_as_unsigned_int(value_id)? {
            return self.build_int_const(val, output_type);
        }

        if curr_output_type.bit_width() <= output_type.bit_width() {
            return Ok(value_id);
        }

        Ok(self.build_single_output_pure(NodeKind::Truncate, [value_id], output_type))
    }

    /// Extends `value_id` to `output_type` using zero- or sign-extension.
    ///
    /// # Errors
    ///
    /// Returns `ExpectedValue` when `value_id` is not a value
    /// edge, or `ExpectedInteger` when `output_type` is not an
    /// integer type and the input is not already a constant we can fold.
    pub fn extend_if_needed(
        &mut self,
        value_id: ValueId,
        output_type: ValueType,
        op: ExtendOp,
    ) -> Result<ValueId> {
        let curr_output_type = self.value_type(value_id)?;

        if let Some((unsigned_val, signed_val)) = self.get_as_int(value_id)? {
            // signed_val is i64; `i64 as u128` sign-extends to fill the
            // high 64 bits, and build_int_const masks to output_type's
            // width.
            return match op {
                ExtendOp::SignExtend => self.build_int_const(signed_val as u128, output_type),
                ExtendOp::ZeroExtend => self.build_int_const(unsigned_val, output_type),
            };
        }

        if !output_type.is_integer() {
            return Err(anyhow!("output {value_id:?} is not an integer value"));
        }

        // Booleans are I1 (integer); the only non-integer input here would be
        // a float, which cannot be width-extended as an integer — it needs an
        // explicit bitcast (`FloatBitsToInt`) first.
        if !curr_output_type.is_integer() {
            return Err(anyhow!(
                "cannot integer-extend non-integer value {value_id:?} \
                 ({curr_output_type}); a bitcast is required first"
            ));
        }

        if curr_output_type.bit_width() == output_type.bit_width() {
            return Ok(value_id);
        }
        if curr_output_type.bit_width() > output_type.bit_width() {
            // Caller asked to extend a value that is already wider than the
            // target.  Truncate so the returned id always carries
            // `output_type`.
            return self.truncate_if_needed(value_id, output_type);
        }
        Ok(self.build_single_output_pure(NodeKind::Extend(op), [value_id], output_type))
    }

    /// Converts `value_id` to integer `output_type`, truncating or
    /// zero-extending as needed.  Keys on **bit width**, so an `I1` boolean
    /// widens to a wider integer via `ZeroExtend` (true→1, false→0) even
    /// though `I1` and `I8` share a byte size.
    ///
    /// # Errors
    ///
    /// Returns an error when `value_id` is not a value edge or carries a
    /// non-integer (float) value.
    pub fn convert_to_int_if_needed(
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

    /// If `input` is not already `float_ty`, converts it: an integer input is
    /// reinterpreted bit-for-bit via `IntBitsToFloat`, and a float of a
    /// different precision is converted via `FloatToFloat`.
    ///
    /// # Errors
    ///
    /// Returns `ExpectedValue` when `input` is not a value edge.
    pub fn cast_to_float_if_needed(
        &mut self,
        input: ValueId,
        float_ty: ValueType,
    ) -> Result<ValueId> {
        let in_ty = self.value_type(input)?;
        if in_ty == float_ty {
            return Ok(input);
        }
        // There is no `CastToFloat` node: an integer input is reinterpreted
        // bit-for-bit as a float of the same width (`IntBitsToFloat`), and a
        // float input of a different precision is converted (`FloatToFloat`).
        // Register reads are always same-width integers, so the lifter takes
        // the `IntBitsToFloat` arm.
        if in_ty.is_float() {
            self.build_float_to_float(input, float_ty)
        } else {
            self.build_int_bits_to_float(input, float_ty)
        }
    }

    /// Infers the float type to use for a value that may be int or float.
    /// If the value is already a float type, that type is used.
    /// For integers, maps byte size: ≤4 → F32, =8 → F64, =10 → F80.
    ///
    /// The 10-byte case targets x87 ST0/STn registers (which the analyzer
    /// represents as I80 on the int side); inferring F80 keeps the
    /// int→float bit-reinterpret round-trip width-preserving.
    ///
    /// # Errors
    ///
    /// Returns `ExpectedValue` when `value` is not a value edge.
    /// Returns an error for an integer input whose byte size has no
    /// corresponding float type (5, 6, 7, 16, 32, 64) — these widths
    /// don't arise from the lifter in practice, and the prior `_ → F64`
    /// catch-all silently bit-truncated them.
    pub fn infer_float_type(&self, value: ValueId) -> Result<ValueType> {
        let ty = self.value_type(value)?;
        if ty.is_float() {
            return Ok(ty);
        }
        match ty.byte_size() {
            0..=4 => Ok(ValueType::F32),
            8 => Ok(ValueType::F64),
            10 => Ok(ValueType::F80),
            other => Err(anyhow!(
                "infer_float_type: integer byte_size {other} has no corresponding \
                 float type (input type: {ty:?})"
            )),
        }
    }
}
