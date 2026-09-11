use strider_ir::{IRBuilderExt, VnTypeExt};

use crate::lift::FunctionLifter;
use crate::lift::pcode_util::{Result, require_output_vn};

impl<R: rsleigh::MemReader> FunctionLifter<'_, R> {
    /// A LOAD from the constant space has its address AS its value: GHIDRA's
    /// `MemoryState::getValue` returns the offset unread for `IPTR_CONSTANT`.
    /// PowerPC exports its `rlwimi` / `rldimi` rotate masks that way.
    pub(super) fn handle_load(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        let space = crate::lift::pcode_util::decode_space_id(insn)?;
        let addr = self.read_input(insn, 1)?;
        let out_vn = require_output_vn(insn)?;
        let out_ty = out_vn.int_type()?;
        let result = if space == rsleigh::VnSpace::CONST {
            self.builder.convert_to_int_if_needed(addr, out_ty)?
        } else {
            self.builder.build_load(addr, space, out_ty)?
        };
        self.write_vn(out_vn, result)
    }

    pub(super) fn handle_store(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        let space = crate::lift::pcode_util::decode_space_id(insn)?;
        let addr = self.read_input(insn, 1)?;
        let data = self.read_input(insn, 2)?;
        self.builder.build_store(addr, data, space)
    }
}
