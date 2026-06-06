//! Memory-load opcode (`Load`).
//!
//! `Store` lives in strider — it advances the memory chain in a way the
//! value lifter doesn't model.

use crate::lift::PerRegionDriver;
use crate::lift::pcode_util::Result;

impl<'a, R: rsleigh::MemReader> PerRegionDriver<'a, R> {
    pub(super) fn handle_load(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        let space = crate::lift::pcode_util::decode_space_id(insn)?;
        let addr = self.read_vn(crate::lift::pcode_util::nth_input_or_err(insn, 1)?)?;
        let out_vn = crate::lift::pcode_util::require_output_vn(insn)?;
        let result = self
            .builder
            .build_load(addr, space, strider_ir::ValueType::int_for_byte_size(out_vn.size)?)?;
        self.write_vn(out_vn, result)
    }
}
