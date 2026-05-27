//! Floating-point value-producing pcode opcodes.
//!
//! Covers float arithmetic (`FloatAdd`, `FloatSub`, `FloatMul`, `FloatDiv`),
//! float unary (`FloatNeg`, `FloatAbs`, `FloatSqrt`, `FloatCeil`,
//! `FloatFloor`, `FloatRound`), float comparisons (`FloatEqual`,
//! `FloatNotEqual`, `FloatLess`, `FloatLessEqual`, `FloatNan`), and
//! float ↔ integer conversions (`FloatInt2Float`, `FloatFloat2Float`,
//! `FloatTrunc`).

use strider_ir::node::NodeOutputType;
use strider_ir::{FloatBinaryOp, FloatCmpOp, FloatUnaryOp};

use crate::pcode_lift::Result;
use crate::pcode_lift::ValueLifter;

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
        float_val: strider_ir::Value,
    ) -> Result<()> {
        let int_ty: NodeOutputType = strider_ir::ValueType::int_for_byte_size(vn.size)?;
        let int_val = self.builder.build_float_bits_to_int(float_val, int_ty)?;
        self.write_vn(vn, int_val)
    }

    /// Translates a float binary p-code instruction into an IR float binary node.
    ///
    /// Reads inputs via `read_vn` (may produce int or float values); the builder
    /// automatically inserts the int→float bitcast (`IntBitsToFloat`) as needed.
    pub(super) fn process_float_binary_op(
        &mut self,
        insn: &rsleigh::Insn,
        op: FloatBinaryOp,
    ) -> Result<()> {
        let lhs = self.read_vn(crate::pcode_lift::nth_input_or_err(insn, 0)?)?;
        let rhs = self.read_vn(crate::pcode_lift::nth_input_or_err(insn, 1)?)?;
        let out_vn = crate::pcode_lift::require_output_vn(insn)?;
        let float_ty = Self::float_type_from_vn(out_vn)?;
        let lhs = self.builder.cast_to_float_if_needed(lhs, float_ty)?;
        let rhs = self.builder.cast_to_float_if_needed(rhs, float_ty)?;
        let result = self.builder.build_float_binary_op(lhs, rhs, op, float_ty)?;
        self.write_float_to_vn(out_vn, result)
    }

    /// Translates a float unary p-code instruction into an IR float unary node.
    pub(super) fn process_float_unary_op(
        &mut self,
        insn: &rsleigh::Insn,
        op: FloatUnaryOp,
    ) -> Result<()> {
        let input = self.read_vn(crate::pcode_lift::nth_input_or_err(insn, 0)?)?;
        let out_vn = crate::pcode_lift::require_output_vn(insn)?;
        let float_ty = Self::float_type_from_vn(out_vn)?;
        let input = self.builder.cast_to_float_if_needed(input, float_ty)?;
        let result = self.builder.build_float_unary_op(input, op, float_ty)?;
        self.write_float_to_vn(out_vn, result)
    }

    /// Translates a float comparison p-code instruction into an IR float cmp node.
    pub(super) fn process_float_cmp_op(
        &mut self,
        insn: &rsleigh::Insn,
        op: FloatCmpOp,
    ) -> Result<()> {
        let lhs = self.read_vn(crate::pcode_lift::nth_input_or_err(insn, 0)?)?;
        let rhs = self.read_vn(crate::pcode_lift::nth_input_or_err(insn, 1)?)?;
        let out_vn = crate::pcode_lift::require_output_vn(insn)?;
        let (lhs, rhs) = self.cast_float_cmp_operands(lhs, rhs)?;
        let result = self.builder.build_float_cmp_op(lhs, rhs, op)?;
        self.write_vn(out_vn, result)
    }

    /// Casts both operands of a float comparison to a single common float
    /// type, reproducing the coercion that `build_float_cmp_op` used to do
    /// internally: the float type is inferred from `lhs`, then both
    /// operands are cast to it.
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

    pub(super) fn handle_float_nan(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        let input = self.read_vn(crate::pcode_lift::nth_input_or_err(insn, 0)?)?;
        let out_vn = crate::pcode_lift::require_output_vn(insn)?;
        // `is_nan(x)` ≡ `x != x` (IEEE 754: NaN ≠ NaN).  Since
        // `FloatCmpOp::NotEqual` is no longer a primitive (lowered at
        // lift to `BoolNeg(FloatEqual)`), build the lowered shape
        // directly: `BoolNeg(FloatEqual(input, input))`.
        let (input, _) = self.cast_float_cmp_operands(input, input)?;
        let eq = self
            .builder
            .build_float_cmp_op(input, input, FloatCmpOp::Equal)?;
        let result = self.builder.build_int_unary_operation(eq, strider_ir::IntUnaryOp::BitNot, strider_ir::ValueType::I1)?;
        self.write_vn(out_vn, result)
    }

    /// Lowers `FloatSub(a, b)` to `FloatAdd(a, FloatUnaryOp::Neg(b))`.
    ///
    /// IEEE 754: `a - b ≡ a + (-b)` for all finite values, and the
    /// negation flips the sign bit on infinities and NaNs without
    /// changing their NaN-ness — so the bit-pattern result matches
    /// `FloatSub` exactly.  Removes the `FloatBinaryOp::Sub` variant
    /// and unifies subtraction with addition for downstream patterns.
    pub(super) fn handle_float_sub(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        let lhs = self.read_vn(crate::pcode_lift::nth_input_or_err(insn, 0)?)?;
        let rhs = self.read_vn(crate::pcode_lift::nth_input_or_err(insn, 1)?)?;
        let out_vn = crate::pcode_lift::require_output_vn(insn)?;
        let float_ty = Self::float_type_from_vn(out_vn)?;
        let lhs = self.builder.cast_to_float_if_needed(lhs, float_ty)?;
        let rhs = self.builder.cast_to_float_if_needed(rhs, float_ty)?;
        let neg_rhs = self.builder.build_float_unary_op(rhs, FloatUnaryOp::Neg, float_ty)?;
        let result = self.builder.build_float_binary_op(lhs, neg_rhs, FloatBinaryOp::Add, float_ty)?;
        self.write_float_to_vn(out_vn, result)
    }

    /// Lowers `FloatNotEqual(a, b)` to `BoolNeg(FloatEqual(a, b))`.
    ///
    /// Sound under IEEE 754: `Equal` is false when either operand is
    /// NaN, so `!Equal` is true (matching the correct `NotEqual` for
    /// NaN inputs).  Mirrors the `IntNotEqual → BoolNeg(IntEqual)`
    /// precedent.
    pub(super) fn handle_float_not_equal(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        let lhs = self.read_vn(crate::pcode_lift::nth_input_or_err(insn, 0)?)?;
        let rhs = self.read_vn(crate::pcode_lift::nth_input_or_err(insn, 1)?)?;
        let out_vn = crate::pcode_lift::require_output_vn(insn)?;
        let (lhs, rhs) = self.cast_float_cmp_operands(lhs, rhs)?;
        let eq = self.builder.build_float_cmp_op(lhs, rhs, FloatCmpOp::Equal)?;
        let result = self.builder.build_int_unary_operation(eq, strider_ir::IntUnaryOp::BitNot, strider_ir::ValueType::I1)?;
        self.write_vn(out_vn, result)
    }

    /// Lowers `FloatLessEqual(a, b)` to `Or(FloatLess(a, b), FloatEqual(a, b))`.
    ///
    /// IEEE 754 requires NaN-aware semantics: `a <= b` returns false
    /// when either operand is NaN, while `BoolNeg(Less(b, a))` would
    /// return true for NaN inputs.  The two-cmp disjunction
    /// (`a < b ∨ a == b`) preserves the correct false-on-NaN result
    /// because both `Less` and `Equal` return false on NaN, so the
    /// `Or` is also false.
    pub(super) fn handle_float_less_equal(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        let lhs = self.read_vn(crate::pcode_lift::nth_input_or_err(insn, 0)?)?;
        let rhs = self.read_vn(crate::pcode_lift::nth_input_or_err(insn, 1)?)?;
        let out_vn = crate::pcode_lift::require_output_vn(insn)?;
        let (lhs, rhs) = self.cast_float_cmp_operands(lhs, rhs)?;
        let lt = self.builder.build_float_cmp_op(lhs, rhs, FloatCmpOp::Less)?;
        let eq = self.builder.build_float_cmp_op(lhs, rhs, FloatCmpOp::Equal)?;
        let result = self.builder.build_int_binary_operation(lt, eq, strider_ir::IntBinaryOp::Or, strider_ir::ValueType::I1)?;
        self.write_vn(out_vn, result)
    }

    pub(super) fn handle_float_int_to_float(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        let raw_input = self.read_vn(crate::pcode_lift::nth_input_or_err(insn, 0)?)?;
        let out_vn = crate::pcode_lift::require_output_vn(insn)?;
        let float_ty = Self::float_type_from_vn(out_vn)?;
        // build_int_to_float requires an integer-typed input.  Register reads
        // are always int-typed (write_float_to_vn round-trips through
        // FloatBitsToInt before storage), so this is usually a no-op — but
        // when the input is an UNIQUE temp left over from a prior float op,
        // cast it back to int first.  `convert_to_int_if_needed` is
        // identity for already-int values and inserts a `FloatBitsToInt`
        // bit-reinterpret otherwise.
        let in_size: NodeOutputType = strider_ir::ValueType::int_for_byte_size(crate::pcode_lift::nth_input_or_err(insn, 0)?.size)?;
        let int_input = self
            .builder
            .convert_to_int_if_needed(raw_input, in_size)?;
        let float_result = self.builder.build_int_to_float(int_input, float_ty)?;
        self.write_float_to_vn(out_vn, float_result)
    }

    pub(super) fn handle_float_float_to_float(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        let raw_input = self.read_vn(crate::pcode_lift::nth_input_or_err(insn, 0)?)?;
        let out_vn = crate::pcode_lift::require_output_vn(insn)?;
        let out_float_ty = Self::float_type_from_vn(out_vn)?;
        // build_float_to_float requires a float-typed input.  Register reads
        // are int-typed, so cast first via the input's natural float width
        // (4-byte → F32, 8-byte → F64).
        let in_float_ty = Self::float_type_from_vn(crate::pcode_lift::nth_input_or_err(insn, 0)?)?;
        let float_input = self.builder.cast_to_float_if_needed(raw_input, in_float_ty)?;
        let float_result = self
            .builder
            .build_float_to_float(float_input, out_float_ty)?;
        self.write_float_to_vn(out_vn, float_result)
    }

    pub(super) fn handle_float_trunc(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        let raw_input = self.read_vn(crate::pcode_lift::nth_input_or_err(insn, 0)?)?;
        let out_vn = crate::pcode_lift::require_output_vn(insn)?;
        let int_ty: NodeOutputType = strider_ir::ValueType::int_for_byte_size(out_vn.size)?;
        // build_float_to_int requires float input.  Cast first via the
        // input's natural float width.
        let in_float_ty = Self::float_type_from_vn(crate::pcode_lift::nth_input_or_err(insn, 0)?)?;
        let float_input = self.builder.cast_to_float_if_needed(raw_input, in_float_ty)?;
        let int_result = self.builder.build_float_to_int(float_input, int_ty)?;
        self.write_vn(out_vn, int_result)
    }
}
