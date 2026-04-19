use crate::error::{ErrorKind, Result};

use super::super::IrAnalyzer;

impl<'a, R: rsleigh::MemReader> IrAnalyzer<'a, R> {
    pub(super) fn handle_copy(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        let input = self.read_vn(&insn.inputs[0])?;
        let out_vn = insn
            .output
            .as_ref()
            .ok_or(ErrorKind::MissingOutputVn(insn.opcode))?;
        self.write_vn(out_vn, input)
    }

    pub(super) fn handle_load(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        let space = insn.inputs[0].addr.space;
        let addr = self.read_vn(&insn.inputs[1])?;
        let out_vn = insn
            .output
            .as_ref()
            .ok_or(ErrorKind::MissingOutputVn(insn.opcode))?;
        let out = self
            .builder
            .build_load(addr, space, out_vn.size.try_into()?)?;
        self.write_vn(out_vn, out)
    }

    pub(super) fn handle_store(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        let space = insn.inputs[0].addr.space;
        let addr = self.read_vn(&insn.inputs[1])?;
        let data = self.read_vn(&insn.inputs[2])?;
        self.builder.build_store(addr, data, space)?;
        Ok(())
    }

    pub(super) fn handle_cast(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        let input = self.read_vn(&insn.inputs[0])?;
        let out_vn = insn
            .output
            .as_ref()
            .ok_or(ErrorKind::MissingOutputVn(insn.opcode))?;
        self.write_vn(out_vn, input)
    }
}
