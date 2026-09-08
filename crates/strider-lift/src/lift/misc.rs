use strider_ir::{IRBuilderExt, VnTypeExt};

use crate::lift::FunctionLifter;
use crate::lift::pcode_util::{Result, require_output_vn};

impl<'a, R: rsleigh::MemReader> FunctionLifter<'a, R> {
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
