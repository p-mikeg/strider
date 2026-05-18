//! Boolean value-producing pcode opcodes: `BoolNeg`, `BoolAnd`, `BoolOr`,
//! `BoolXor`.

use strider_ir::{BoolBinaryOp, BoolUnaryOp};

use crate::pcode_lift::Result;
use crate::pcode_lift::ValueLifter;

impl<'a, R: rsleigh::MemReader> ValueLifter<'a, R> {
    /// Translates a p-code boolean binary instruction into an IR boolean
    /// operation node and writes the result to the output varnode.
    pub(super) fn process_bool_binary_op(
        &mut self,
        insn: &rsleigh::Insn,
        op: BoolBinaryOp,
    ) -> Result<()> {
        let lhs = self.read_vn(&insn.inputs[0])?;
        let rhs = self.read_vn(&insn.inputs[1])?;
        let out_vn = crate::pcode_lift::require_output_vn(insn)?;
        let out = self.builder.build_boolean_operation(lhs, rhs, op)?;
        self.write_vn(out_vn, out)
    }

    /// Translates a p-code boolean unary instruction into an IR boolean
    /// unary node and writes the result to the output varnode.
    pub(super) fn process_bool_unary_op(
        &mut self,
        insn: &rsleigh::Insn,
        op: BoolUnaryOp,
    ) -> Result<()> {
        let input = self.read_vn(&insn.inputs[0])?;
        let out_vn = crate::pcode_lift::require_output_vn(insn)?;
        let out = self.builder.build_boolean_unary_operation(input, op)?;
        self.write_vn(out_vn, out)
    }
}
