use anyhow::anyhow;

use super::FunctionBuilder;
use crate::error::Result;
use crate::node::{NodeKind, NodeOutputId, NodeOutputType};
use crate::ops::ExtendOp;

/// Unified return shape for [`FunctionBuilder::const_value`].
///
/// `Int { val, ty }` carries the raw `u128` payload of an `IntConst`
/// node alongside its declared `NodeOutputType` so callers can decide
/// whether to view it unsigned / signed / mask / etc.  `Float` carries
/// the raw bit pattern of a `FloatConst` — the analyzer never needs
/// the float type for constant folding (`f32` vs `f64` is inferred
/// from the surrounding op), so the type isn't carried here.
#[derive(Debug, Clone, Copy)]
pub(crate) enum ConstValue {
    Bool(bool),
    Int { val: u128, ty: NodeOutputType },
    Float { bits: u64 },
}

impl FunctionBuilder {
    /// Retrieves the [`NodeOutputType`] of `output_id`.
    ///
    /// Returns an error if the output does not carry a value (e.g. it is a
    /// control or memory edge).
    ///
    /// # Errors
    ///
    /// Returns `ExpectedValue` when `output_id` is a control,
    /// memory, or control-phi edge.
    pub fn get_output_type(&self, output_id: NodeOutputId) -> Result<NodeOutputType> {
        let kind = self.function().output_kind(output_id);
        kind.as_value()
            .ok_or_else(|| anyhow!("output {output_id:?} is not a value edge (got {kind:?})"))
    }

    /// Returns the constant value carried by `output_id` if its defining
    /// node is `IntConst`, `BoolConst`, or `FloatConst`; `Ok(None)`
    /// otherwise.  The five `get_as_*` helpers below are thin
    /// projections off this unified shape.
    ///
    /// # Errors
    ///
    /// Returns `ExpectedValue` when `output_id` is not a value edge.
    pub(crate) fn const_value(&self, output_id: NodeOutputId) -> Result<Option<ConstValue>> {
        let ty = self.get_output_type(output_id)?;
        Ok(match self.function().kind_of_output(output_id) {
            NodeKind::IntConst(val) if ty.is_integer() => Some(ConstValue::Int { val: *val, ty }),
            NodeKind::BoolConst(val) if ty.is_bool() => Some(ConstValue::Bool(*val)),
            NodeKind::FloatConst(bits) if ty.is_float() => {
                Some(ConstValue::Float { bits: *bits })
            }
            _ => None,
        })
    }

    /// If `output_id` is a constant node, returns its value as a `bool`.
    ///
    /// Returns `Ok(None)` for non-constant nodes.  An `IntConst` is considered
    /// `true` when non-zero.  Returns an error if the output is not a value.
    ///
    /// # Errors
    ///
    /// Returns `ExpectedValue` when `output_id` is not a value
    /// edge.
    pub(crate) fn get_as_bool(&self, output_id: NodeOutputId) -> Result<Option<bool>> {
        Ok(self.const_value(output_id)?.and_then(|c| match c {
            ConstValue::Bool(b) => Some(b),
            ConstValue::Int { val, .. } => Some(val != 0),
            ConstValue::Float { .. } => None,
        }))
    }

    /// Converts `output_id` to a boolean output, inserting a `CastToBool`
    /// node if needed.
    ///
    /// # Errors
    ///
    /// Returns `ExpectedValue` when `output_id` is not a value
    /// edge.
    pub fn convert_to_bool_if_needed(&mut self, output_id: NodeOutputId) -> Result<NodeOutputId> {
        let output_kind = self.function().output_kind(output_id);
        if !output_kind.is_value() {
            return Err(anyhow!(
                "output {output_id:?} is not a value edge (got {output_kind:?})"
            ));
        }

        if let Some(bool_val) = self.get_as_bool(output_id)? {
            return Ok(self.build_boolean_const(bool_val));
        }

        if output_kind.as_value() == Some(NodeOutputType::Bool) {
            return Ok(output_id);
        }

        Ok(self.build_single_output_pure(NodeKind::CastToBool, [output_id], NodeOutputType::Bool))
    }

    /// If `output_id` is a constant node, returns its value truncated to the
    /// declared [`NodeOutputType`] as an unsigned 64-bit integer.
    ///
    /// Returns `Ok(None)` for non-constant nodes.
    ///
    /// # Errors
    ///
    /// Returns `ExpectedValue` when `output_id` is not a value
    /// edge.
    pub fn get_as_unsigned_int(&self, output_id: NodeOutputId) -> Result<Option<u64>> {
        Ok(self.const_value(output_id)?.and_then(|c| match c {
            ConstValue::Int { val, ty } => {
                ty.get_unsigned_int(val).and_then(|v| u64::try_from(v).ok())
            }
            ConstValue::Bool(b) => Some(b as u64),
            ConstValue::Float { .. } => None,
        }))
    }

    /// If `output_id` is an integer or bool constant, returns its value
    /// sign-extended to `i64` according to the declared [`NodeOutputType`].
    ///
    /// `BoolConst(true)` returns `Some(1)`; `BoolConst(false)` returns
    /// `Some(0)` — consistent with [`Self::get_as_unsigned_int`], so that
    /// [`Self::get_as_int`] can fold Bool constants in `extend_if_needed`.
    ///
    /// Returns `Ok(None)` for non-constant nodes.
    ///
    /// # Errors
    ///
    /// Returns `ExpectedValue` when `output_id` is not a value
    /// edge.
    pub fn get_as_signed_int(&self, output_id: NodeOutputId) -> Result<Option<i64>> {
        Ok(self.const_value(output_id)?.and_then(|c| match c {
            ConstValue::Int { val, ty } => {
                ty.get_signed_int(val).and_then(|v| i64::try_from(v).ok())
            }
            ConstValue::Bool(b) => Some(i64::from(b)),
            ConstValue::Float { .. } => None,
        }))
    }

    /// Returns both the unsigned and signed interpretations of `output_id` if
    /// it is an integer constant, or `None` otherwise.
    ///
    /// # Errors
    ///
    /// Returns `ExpectedValue` when `output_id` is not a value
    /// edge.
    pub fn get_as_int(&self, output_id: NodeOutputId) -> Result<Option<(u64, i64)>> {
        Ok(self.get_as_unsigned_int(output_id)?.zip(self.get_as_signed_int(output_id)?))
    }

    /// If `output_id` is a `FloatConst` node, returns its raw bit pattern.
    /// Returns `Ok(None)` for non-constant nodes.
    ///
    /// # Errors
    ///
    /// Returns `ExpectedValue` when `output_id` is not a value
    /// edge.
    pub fn get_as_float_bits(&self, output_id: NodeOutputId) -> Result<Option<u64>> {
        Ok(self.const_value(output_id)?.and_then(|c| match c {
            ConstValue::Float { bits } => Some(bits),
            _ => None,
        }))
    }

    /// Truncates `output_id` to `output_type` if it is currently wider.
    ///
    /// # Errors
    ///
    /// Returns `ExpectedValue` when `output_id` is not a value
    /// edge.
    pub fn truncate_if_needed(
        &mut self,
        output_id: NodeOutputId,
        output_type: NodeOutputType,
    ) -> Result<NodeOutputId> {
        let curr_output_type = self.get_output_type(output_id)?;

        if let Some(val) = self.get_as_unsigned_int(output_id)? {
            return self.build_int_const(val, output_type);
        }

        if curr_output_type.byte_size() <= output_type.byte_size() {
            return Ok(output_id);
        }

        Ok(self.build_single_output_pure(NodeKind::Truncate, [output_id], output_type))
    }

    /// Extends `output_id` to `output_type` using zero- or sign-extension.
    ///
    /// # Errors
    ///
    /// Returns `ExpectedValue` when `output_id` is not a value
    /// edge, or `ExpectedInteger` when `output_type` is not an
    /// integer type and the input is not already a constant we can fold.
    pub fn extend_if_needed(
        &mut self,
        output_id: NodeOutputId,
        output_type: NodeOutputType,
        op: ExtendOp,
    ) -> Result<NodeOutputId> {
        let curr_output_type = self.get_output_type(output_id)?;

        if let Some((unsigned_val, signed_val)) = self.get_as_int(output_id)? {
            // signed_val is i64; `i64 as u128` sign-extends to fill the
            // high 64 bits, and build_int_const masks to output_type's
            // width.
            return match op {
                ExtendOp::SignExtend => self.build_int_const(signed_val as u128, output_type),
                ExtendOp::ZeroExtend => self.build_int_const(unsigned_val, output_type),
            };
        }

        if !output_type.is_integer() {
            return Err(anyhow!("output {output_id:?} is not an integer value"));
        }

        // Non-integer input (Bool / Float) into an integer extend: insert a
        // CastToInt first so the Extend node receives an AnyInt input as its
        // signature requires.  Without this, comparison results (Bool) flowing
        // through register writes via write_reg_vn would fail IR validation
        // with "OutputType(Bool), expected AnyInt".
        if !curr_output_type.is_integer() {
            return self.convert_to_int_if_needed(output_id, output_type);
        }

        if curr_output_type.byte_size() == output_type.byte_size() {
            return Ok(output_id);
        }
        if curr_output_type.byte_size() > output_type.byte_size() {
            // Caller asked to extend a value that is already wider than the
            // target.  Truncate so the returned id always carries
            // `output_type`.
            return self.truncate_if_needed(output_id, output_type);
        }
        Ok(self.build_single_output_pure(NodeKind::Extend(op), [output_id], output_type))
    }

    /// Converts `output_id` to `output_type`, truncating or zero-extending as needed.
    ///
    /// # Errors
    ///
    /// Returns `ExpectedValue` when `output_id` is not a value
    /// edge.
    pub fn convert_to_int_if_needed(
        &mut self,
        output_id: NodeOutputId,
        output_type: NodeOutputType,
    ) -> Result<NodeOutputId> {
        let curr_output_type = self.get_output_type(output_id)?;
        if curr_output_type.is_integer() {
            let truncate_id = self.truncate_if_needed(output_id, output_type)?;
            let extend_id =
                self.extend_if_needed(truncate_id, output_type, ExtendOp::ZeroExtend)?;
            return Ok(extend_id);
        }
        Ok(self.build_single_output_pure(NodeKind::CastToInt, [output_id], output_type))
    }

    /// If `input` is not already `float_ty`, wraps it in a `CastToFloat` node.
    ///
    /// # Errors
    ///
    /// Returns `ExpectedValue` when `input` is not a value edge.
    pub fn cast_to_float_if_needed(
        &mut self,
        input: NodeOutputId,
        float_ty: NodeOutputType,
    ) -> Result<NodeOutputId> {
        if self.get_output_type(input)? == float_ty {
            return Ok(input);
        }
        Ok(self.build_cast_to_float(input, float_ty))
    }

    /// Infers the float type to use for a value that may be int or float.
    /// If the value is already a float type, that type is used.
    /// For integers, maps byte size: ≤4 → F32, =8 → F64, =10 → F80.
    ///
    /// The 10-byte case targets x87 ST0/STn registers (which the analyzer
    /// represents as I80 on the int side); inferring F80 keeps the
    /// `CastToFloat` round-trip width-preserving.
    ///
    /// # Errors
    ///
    /// Returns `ExpectedValue` when `input` is not a value edge.
    /// Returns an error for an integer input whose byte size has no
    /// corresponding float type (5, 6, 7, 16, 32, 64) — these widths
    /// don't arise from the lifter in practice, and the prior `_ → F64`
    /// catch-all silently bit-truncated them.
    pub fn infer_float_type(&self, input: NodeOutputId) -> Result<NodeOutputType> {
        let ty = self.get_output_type(input)?;
        if ty.is_float() {
            return Ok(ty);
        }
        match ty.byte_size() {
            0..=4 => Ok(NodeOutputType::F32),
            8 => Ok(NodeOutputType::F64),
            10 => Ok(NodeOutputType::F80),
            other => Err(anyhow!(
                "infer_float_type: integer byte_size {other} has no corresponding \
                 float type (input type: {ty:?})"
            )),
        }
    }
}
