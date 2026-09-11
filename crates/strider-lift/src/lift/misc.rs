use strider_ir::{IRBuilderExt, VnTypeExt};

use crate::lift::FunctionLifter;
use crate::lift::pcode_util::{Result, nth_input_or_err, require_output_vn};

impl<'a, R: rsleigh::MemReader> FunctionLifter<'a, R> {
    /// inputs are (CONST op id, segment, offset).
    pub(super) fn handle_segment_op(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        let id_vn = nth_input_or_err(insn, 0)?;
        crate::lift::pcode_util::ensure_const_space(id_vn, insn.opcode, "input 0")?;
        let op_id = id_vn.addr_off;
        let segment = self.read_input(insn, 1)?;
        let offset = self.read_input(insn, 2)?;
        let out_vn = require_output_vn(insn)?;
        let result = self
            .builder
            .build_segment_op(op_id, segment, offset, out_vn.int_type()?)?;
        self.write_vn(out_vn, result)
    }

    pub(super) fn handle_cpool_ref(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        let refs = self.read_vns(&insn.inputs)?;
        let out_vn = require_output_vn(insn)?;
        let result = self.builder.build_cpool_ref(&refs, out_vn.int_type()?)?;
        self.write_vn(out_vn, result)
    }

    pub(super) fn handle_new(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        let args = self.read_vns(&insn.inputs)?;
        let out_vn = require_output_vn(insn)?;
        let result = self.builder.build_new(&args, out_vn.int_type()?)?;
        self.write_vn(out_vn, result)
    }
}
