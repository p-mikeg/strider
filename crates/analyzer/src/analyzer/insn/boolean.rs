use ir::{BoolBinaryOp, BoolUnaryOp};

use crate::error::{ErrorKind, Result};

use super::super::IrAnalyzer;

impl<'a, R: rsleigh::MemReader> IrAnalyzer<'a, R> {
    /// Translates a p-code boolean binary instruction into an IR boolean
    /// operation node and writes the result to the output varnode.
    pub(super) fn process_bool_binary_op(
        &mut self,
        insn: &rsleigh::Insn,
        op: BoolBinaryOp,
    ) -> Result<()> {
        let lhs = self.read_vn(&insn.inputs[0])?;
        let rhs = self.read_vn(&insn.inputs[1])?;
        let out_vn = insn
            .output
            .as_ref()
            .ok_or(ErrorKind::MissingOutputVn(insn.opcode))?;
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
        let out_vn = insn
            .output
            .as_ref()
            .ok_or(ErrorKind::MissingOutputVn(insn.opcode))?;
        let out = self.builder.build_boolean_unary_operation(input, op)?;
        self.write_vn(out_vn, out)
    }
}
