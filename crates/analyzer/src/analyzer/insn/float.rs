use ir::node::NodeOutputType;
use ir::{FloatBinaryOp, FloatCmpOp, FloatUnaryOp};

use crate::error::{ErrorKind, Result};

use super::super::IrAnalyzer;

impl<'a, R: rsleigh::MemReader> IrAnalyzer<'a, R> {
    // ── Float helpers ─────────────────────────────────────────────────────────

    /// Maps a varnode byte size to the corresponding float [`NodeOutputType`].
    /// Returns an error for sizes other than 4 (F32) or 8 (F64).
    pub(super) fn float_type_from_vn(vn: &rsleigh::Vn) -> Result<NodeOutputType> {
        match vn.size {
            4 => Ok(NodeOutputType::F32),
            8 => Ok(NodeOutputType::F64),
            n => Err(ErrorKind::UnsupportedFloatSize(n).into()),
        }
    }

    /// Bitcasts a float result back to an integer of the same width and writes
    /// it to the output varnode (float results in registers are stored as ints).
    pub(super) fn write_float_to_vn(
        &mut self,
        vn: &rsleigh::Vn,
        float_val: ir::Value,
    ) -> Result<()> {
        let int_ty: NodeOutputType = vn.size.try_into()?;
        let int_val = self.builder.build_float_bits_to_int(float_val, int_ty)?;
        self.write_vn(vn, int_val)
    }

    /// Translates a float binary p-code instruction into an IR float binary node.
    ///
    /// Reads inputs via `read_vn` (may produce int or float values); the builder
    /// automatically inserts `CastToFloat` nodes as needed.
    pub(super) fn process_float_binary_op(
        &mut self,
        insn: &rsleigh::Insn,
        op: FloatBinaryOp,
    ) -> Result<()> {
        let lhs = self.read_vn(&insn.inputs[0])?;
        let rhs = self.read_vn(&insn.inputs[1])?;
        let out_vn = super::require_output_vn(insn)?;
        let float_ty = Self::float_type_from_vn(out_vn)?;
        let result = self.builder.build_float_binary_op(lhs, rhs, op, float_ty)?;
        self.write_float_to_vn(out_vn, result)
    }

    /// Translates a float unary p-code instruction into an IR float unary node.
    pub(super) fn process_float_unary_op(
        &mut self,
        insn: &rsleigh::Insn,
        op: FloatUnaryOp,
    ) -> Result<()> {
        let input = self.read_vn(&insn.inputs[0])?;
        let out_vn = super::require_output_vn(insn)?;
        let float_ty = Self::float_type_from_vn(out_vn)?;
        let result = self.builder.build_float_unary_op(input, op, float_ty)?;
        self.write_float_to_vn(out_vn, result)
    }

    /// Translates a float comparison p-code instruction into an IR float cmp node.
    pub(super) fn process_float_cmp_op(
        &mut self,
        insn: &rsleigh::Insn,
        op: FloatCmpOp,
    ) -> Result<()> {
        let lhs = self.read_vn(&insn.inputs[0])?;
        let rhs = self.read_vn(&insn.inputs[1])?;
        let out_vn = super::require_output_vn(insn)?;
        let result = self.builder.build_float_cmp_op(lhs, rhs, op)?;
        self.write_vn(out_vn, result)
    }

    pub(super) fn handle_float_nan(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        let input = self.read_vn(&insn.inputs[0])?;
        let out_vn = super::require_output_vn(insn)?;
        let result = self
            .builder
            .build_float_cmp_op(input, input, FloatCmpOp::NotEqual)?;
        self.write_vn(out_vn, result)
    }

    pub(super) fn handle_float_int_to_float(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        let int_input = self.read_vn(&insn.inputs[0])?;
        let out_vn = super::require_output_vn(insn)?;
        let float_ty = Self::float_type_from_vn(out_vn)?;
        let float_result = self.builder.build_int_to_float(int_input, float_ty)?;
        self.write_float_to_vn(out_vn, float_result)
    }

    pub(super) fn handle_float_float_to_float(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        let float_input = self.read_vn(&insn.inputs[0])?;
        let out_vn = super::require_output_vn(insn)?;
        let out_float_ty = Self::float_type_from_vn(out_vn)?;
        let float_result = self
            .builder
            .build_float_to_float(float_input, out_float_ty)?;
        self.write_float_to_vn(out_vn, float_result)
    }

    pub(super) fn handle_float_trunc(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        let float_input = self.read_vn(&insn.inputs[0])?;
        let out_vn = super::require_output_vn(insn)?;
        let int_ty: NodeOutputType = out_vn.size.try_into()?;
        let int_result = self.builder.build_float_to_int(float_input, int_ty)?;
        self.write_vn(out_vn, int_result)
    }
}
