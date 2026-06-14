//! Boolean value-producing pcode opcodes: `BoolNeg`, `BoolAnd`, `BoolOr`,
//! `BoolXor`.
//!
//! Booleans are modelled as the 1-bit integer `I1`, so these lower to
//! ordinary integer operations: `BoolAnd`/`BoolOr`/`BoolXor` →
//! `IntBinaryOp::{And,Or,Xor}` at `I1`, and `BoolNeg` (logical not of a
//! 1-bit value) → `Xor(x, IntConst(1)):I1` (the I1 all-ones constant is 1,
//! and `x ^ 1` flips the single bit).  Sleigh always feeds these ops
//! already-`I1` operands (comparison / flag results), so no int→bool
//! conversion is needed.

use strider_ir::IRBuilderExt;
use strider_ir::{IntBinaryOp, ValueType};

use crate::lift::FunctionLifter;
use crate::lift::pcode_util::Result;

impl<'a, R: rsleigh::MemReader> FunctionLifter<'a, R> {
    /// Translates a p-code boolean binary instruction into an `I1` integer
    /// binary operation node and writes the result to the output varnode.
    pub(super) fn process_bool_binary_op(
        &mut self,
        insn: &rsleigh::Insn,
        op: IntBinaryOp,
    ) -> Result<()> {
        let lhs = self.read_vn(crate::lift::pcode_util::nth_input_or_err(insn, 0)?)?;
        let rhs = self.read_vn(crate::lift::pcode_util::nth_input_or_err(insn, 1)?)?;
        let out_vn = crate::lift::pcode_util::require_output_vn(insn)?;
        let lhs = self.builder.convert_to_int_if_needed(lhs, ValueType::I1)?;
        let rhs = self.builder.convert_to_int_if_needed(rhs, ValueType::I1)?;
        let result = self
            .builder
            .build_int_binary_operation(lhs, rhs, op, ValueType::I1)?;
        self.write_vn(out_vn, result)
    }

    /// Translates a p-code boolean unary instruction (`BoolNeg`) into an `I1`
    /// `Xor(x, IntConst(1)):I1` node and writes the result to the output varnode.
    ///
    /// `BoolNeg` is logical NOT of a 1-bit value.  Since the former BitNot unary-op
    /// was removed in favour of `Xor(x, all_ones)`, a 1-bit complement is
    /// `Xor(x, IntConst(1))` at `I1` (the I1 all-ones constant is 1).
    pub(super) fn process_bool_unary_op(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        let value = self.read_vn(crate::lift::pcode_util::nth_input_or_err(insn, 0)?)?;
        let out_vn = crate::lift::pcode_util::require_output_vn(insn)?;
        let value = self
            .builder
            .convert_to_int_if_needed(value, ValueType::I1)?;
        let all_ones = self.builder.build_int_const(u128::MAX, ValueType::I1)?;
        let result = self.builder.build_int_binary_operation(
            value,
            all_ones,
            IntBinaryOp::Xor,
            ValueType::I1,
        )?;
        self.write_vn(out_vn, result)
    }
}
