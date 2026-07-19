//! Memory opcodes.  `Load` produces a value; `Store` advances the unified
//! memory chain.

use strider_ir::VnTypeExt;

use crate::lift::FunctionLifter;
use crate::lift::pcode_util::{Result, require_output_vn};

impl<R: rsleigh::MemReader> FunctionLifter<'_, R> {
    pub(super) fn handle_load(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        let space = crate::lift::pcode_util::decode_space_id(insn)?;
        let addr = self.read_input(insn, 1)?;
        let out_vn = require_output_vn(insn)?;
        let result = self.builder.build_load(addr, space, out_vn.int_type()?)?;
        self.write_vn(out_vn, result)
    }

    pub(super) fn handle_store(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        let space = crate::lift::pcode_util::decode_space_id(insn)?;
        let addr = self.read_input(insn, 1)?;
        let data = self.read_input(insn, 2)?;
        self.builder.build_store(addr, data, space)
    }
}
