use super::FunctionBuilder;
use crate::error::{ErrorKind, Result};
use crate::node::{NodeKind, NodeOutputId, NodeOutputType};
use crate::ops::ExtendOp;

impl FunctionBuilder {
    /// Retrieves the [`NodeOutputType`] of `output_id`.
    ///
    /// Returns an error if the output does not carry a value (e.g. it is a
    /// control or memory edge).
    ///
    /// # Errors
    ///
    /// Returns [`ErrorKind::ExpectedValue`] when `output_id` is a control,
    /// memory, or control-phi edge.
    pub fn get_output_type(&self, output_id: NodeOutputId) -> Result<NodeOutputType> {
        let kind = self.graph().output_kind(output_id);
        kind.as_value()
            .ok_or_else(|| ErrorKind::ExpectedValue(output_id, kind).into())
    }

    /// If `output_id` is a constant node, returns its value as a `bool`.
    ///
    /// Returns `Ok(None)` for non-constant nodes.  An `IntConst` is considered
    /// `true` when non-zero.  Returns an error if the output is not a value.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorKind::ExpectedValue`] when `output_id` is not a value
    /// edge.
    pub fn get_as_bool(&self, output_id: NodeOutputId) -> Result<Option<bool>> {
        let output_type = self.get_output_type(output_id)?;
        let node_id = self.graph().get_node_from_output(output_id);
        match self.graph().node_kind(node_id) {
            NodeKind::IntConst(val) if output_type.is_integer() => Ok(Some(*val != 0)),
            NodeKind::BoolConst(val) if output_type.is_bool() => Ok(Some(*val)),
            _ => Ok(None),
        }
    }

    /// Converts `output_id` to a boolean output, inserting a `CastToBool`
    /// node if needed.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorKind::ExpectedValue`] when `output_id` is not a value
    /// edge.
    pub fn convert_to_bool_if_needed(&mut self, output_id: NodeOutputId) -> Result<NodeOutputId> {
        let output_kind = self.graph().output_kind(output_id);
        if !output_kind.is_value() {
            return Err(ErrorKind::ExpectedValue(output_id, output_kind).into());
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
    /// Returns [`ErrorKind::ExpectedValue`] when `output_id` is not a value
    /// edge.
    pub fn get_as_unsigned_int(&self, output_id: NodeOutputId) -> Result<Option<u64>> {
        let output_type = self.get_output_type(output_id)?;
        let node_id = self.graph().get_node_from_output(output_id);
        match self.graph().node_kind(node_id) {
            NodeKind::IntConst(val) if output_type.is_integer() => {
                Ok(output_type.get_unsigned_int_u128(*val).and_then(|v| u64::try_from(v).ok()))
            }
            NodeKind::BoolConst(val) if output_type.is_bool() => Ok(Some(*val as u64)),
            _ => Ok(None),
        }
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
    /// Returns [`ErrorKind::ExpectedValue`] when `output_id` is not a value
    /// edge.
    pub fn get_as_signed_int(&self, output_id: NodeOutputId) -> Result<Option<i64>> {
        let output_type = self.get_output_type(output_id)?;
        let node_id = self.graph().get_node_from_output(output_id);
        match self.graph().node_kind(node_id) {
            NodeKind::IntConst(val) if output_type.is_integer() => {
                Ok(output_type.get_signed_int_i128(*val).and_then(|v| i64::try_from(v).ok()))
            }
            NodeKind::BoolConst(val) if output_type.is_bool() => Ok(Some(i64::from(*val))),
            _ => Ok(None),
        }
    }

    /// Returns both the unsigned and signed interpretations of `output_id` if
    /// it is an integer constant, or `None` otherwise.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorKind::ExpectedValue`] when `output_id` is not a value
    /// edge.
    pub fn get_as_int(&self, output_id: NodeOutputId) -> Result<Option<(u64, i64)>> {
        let unsigned_val = self.get_as_unsigned_int(output_id)?;
        let signed_val = self.get_as_signed_int(output_id)?;
        match (unsigned_val, signed_val) {
            (Some(u), Some(s)) => Ok(Some((u, s))),
            _ => Ok(None),
        }
    }

    /// If `output_id` is a `FloatConst` node, returns its raw bit pattern.
    /// Returns `Ok(None)` for non-constant nodes.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorKind::ExpectedValue`] when `output_id` is not a value
    /// edge.
    pub fn get_as_float_bits(&self, output_id: NodeOutputId) -> Result<Option<u64>> {
        let output_type = self.get_output_type(output_id)?;
        if !output_type.is_float() {
            return Ok(None);
        }
        let node_id = self.graph().get_node_from_output(output_id);
        match self.graph().node_kind(node_id) {
            NodeKind::FloatConst(bits) => Ok(Some(*bits)),
            _ => Ok(None),
        }
    }

    /// Truncates `output_id` to `output_type` if it is currently wider.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorKind::ExpectedValue`] when `output_id` is not a value
    /// edge.
    pub fn truncate_if_needed(
        &mut self,
        output_id: NodeOutputId,
        output_type: NodeOutputType,
    ) -> Result<NodeOutputId> {
        let curr_output_type = self.get_output_type(output_id)?;

        if let Some(val) = self.get_as_unsigned_int(output_id)? {
            return Ok(self.build_int_const(val, output_type));
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
    /// Returns [`ErrorKind::ExpectedValue`] when `output_id` is not a value
    /// edge, or [`ErrorKind::ExpectedInteger`] when `output_type` is not an
    /// integer type and the input is not already a constant we can fold.
    pub fn extend_if_needed(
        &mut self,
        output_id: NodeOutputId,
        output_type: NodeOutputType,
        op: ExtendOp,
    ) -> Result<NodeOutputId> {
        let curr_output_type = self.get_output_type(output_id)?;

        if let Some((unsigned_val, signed_val)) = self.get_as_int(output_id)? {
            return Ok(match op {
                // signed_val is i64; reinterpret bits as u128 (sign-extended to i128 then cast)
                ExtendOp::SignExtend => self.build_int_const(signed_val as u128, output_type),
                ExtendOp::ZeroExtend => self.build_int_const(unsigned_val, output_type),
            });
        }

        if !output_type.is_integer() {
            return Err(ErrorKind::ExpectedInteger(output_id).into());
        }

        // Non-integer input (Bool / Float) into an integer extend: insert a
        // CastToInt first so the Extend node receives an AnyInt input as its
        // signature requires.  Without this, comparison results (Bool) flowing
        // through register writes via write_reg_vn would fail IR validation
        // with "OutputType(Bool), expected AnyInt".
        if !curr_output_type.is_integer() {
            return self.convert_to_int_if_needed(output_id, output_type);
        }

        if curr_output_type.byte_size() >= output_type.byte_size() {
            return Ok(output_id);
        }
        Ok(self.build_single_output_pure(NodeKind::Extend(op), [output_id], output_type))
    }

    /// Converts `output_id` to `output_type`, truncating or zero-extending as needed.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorKind::ExpectedValue`] when `output_id` is not a value
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
    pub(super) fn cast_to_float_if_needed(
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
    /// For integers, maps byte size: ≤4 → F32, otherwise → F64.
    pub(super) fn infer_float_type(&self, input: NodeOutputId) -> Result<NodeOutputType> {
        let ty = self.get_output_type(input)?;
        if ty.is_float() {
            return Ok(ty);
        }
        Ok(if ty.byte_size() <= 4 {
            NodeOutputType::F32
        } else {
            NodeOutputType::F64
        })
    }
}
