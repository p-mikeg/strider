//! Floating-point value-producing pcode opcodes.
//!
//! Covers float arithmetic (`FloatAdd`, `FloatSub`, `FloatMul`, `FloatDiv`),
//! float unary (`FloatNeg`, `FloatAbs`, `FloatSqrt`, `FloatCeil`,
//! `FloatFloor`, `FloatRound`), float comparisons (`FloatEqual`,
//! `FloatNotEqual`, `FloatLess`, `FloatLessEqual`, `FloatNan`), and
//! float ↔ integer conversions (`FloatInt2Float`, `FloatFloat2Float`,
//! `FloatTrunc`).

use ir::node::NodeOutputType;
use ir::{FloatBinaryOp, FloatCmpOp, FloatUnaryOp};

use crate::Result;
use crate::ValueLifter;

impl<'a, R: rsleigh::MemReader> ValueLifter<'a, R> {
    /// Maps a varnode byte size to the corresponding float [`NodeOutputType`].
    /// Delegates to [`NodeOutputType::float_for_byte_size`].
    pub(super) fn float_type_from_vn(vn: &rsleigh::Vn) -> Result<NodeOutputType> {
        NodeOutputType::float_for_byte_size(vn.size)
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
        let out_vn = crate::require_output_vn(insn)?;
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
        let out_vn = crate::require_output_vn(insn)?;
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
        let out_vn = crate::require_output_vn(insn)?;
        let result = self.builder.build_float_cmp_op(lhs, rhs, op)?;
        self.write_vn(out_vn, result)
    }

    pub(super) fn handle_float_nan(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        let input = self.read_vn(&insn.inputs[0])?;
        let out_vn = crate::require_output_vn(insn)?;
        let result = self
            .builder
            .build_float_cmp_op(input, input, FloatCmpOp::NotEqual)?;
        self.write_vn(out_vn, result)
    }

    pub(super) fn handle_float_int_to_float(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        let raw_input = self.read_vn(&insn.inputs[0])?;
        let out_vn = crate::require_output_vn(insn)?;
        let float_ty = Self::float_type_from_vn(out_vn)?;
        // build_int_to_float requires an integer-typed input.  Register reads
        // are always int-typed (write_float_to_vn round-trips through
        // FloatBitsToInt before storage), so this is usually a no-op — but
        // when the input is an UNIQUE temp left over from a prior float op,
        // cast it back to int first.  `convert_to_int_if_needed` is
        // identity for already-int values and inserts CastToInt otherwise.
        let in_size: NodeOutputType = insn.inputs[0].size.try_into()?;
        let int_input = self
            .builder
            .convert_to_int_if_needed(raw_input, in_size)?;
        let float_result = self.builder.build_int_to_float(int_input, float_ty)?;
        self.write_float_to_vn(out_vn, float_result)
    }

    pub(super) fn handle_float_float_to_float(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        let raw_input = self.read_vn(&insn.inputs[0])?;
        let out_vn = crate::require_output_vn(insn)?;
        let out_float_ty = Self::float_type_from_vn(out_vn)?;
        // build_float_to_float requires a float-typed input.  Register reads
        // are int-typed, so cast first via the input's natural float width
        // (4-byte → F32, 8-byte → F64).
        let in_float_ty = Self::float_type_from_vn(&insn.inputs[0])?;
        let float_input = self.builder.cast_to_float_if_needed(raw_input, in_float_ty)?;
        let float_result = self
            .builder
            .build_float_to_float(float_input, out_float_ty)?;
        self.write_float_to_vn(out_vn, float_result)
    }

    pub(super) fn handle_float_trunc(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        let raw_input = self.read_vn(&insn.inputs[0])?;
        let out_vn = crate::require_output_vn(insn)?;
        let int_ty: NodeOutputType = out_vn.size.try_into()?;
        // build_float_to_int requires float input.  Cast first via the
        // input's natural float width.
        let in_float_ty = Self::float_type_from_vn(&insn.inputs[0])?;
        let float_input = self.builder.cast_to_float_if_needed(raw_input, in_float_ty)?;
        let int_result = self.builder.build_float_to_int(float_input, int_ty)?;
        self.write_vn(out_vn, int_result)
    }
}
