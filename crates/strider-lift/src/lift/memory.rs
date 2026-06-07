//! Memory opcodes: `Load` and `Store`.
//!
//! `Load` is value-producing; `Store` advances the unified memory chain.
//! Both decode their address space via `pcode_util::decode_space_id`.

use crate::lift::PerRegionDriver;
use crate::lift::pcode_util::{Result, nth_input_or_err};

impl<R: rsleigh::MemReader> PerRegionDriver<'_, R> {
    pub(super) fn handle_load(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        let space = crate::lift::pcode_util::decode_space_id(insn)?;
        let addr = self.read_vn(crate::lift::pcode_util::nth_input_or_err(insn, 1)?)?;
        let out_vn = crate::lift::pcode_util::require_output_vn(insn)?;
        let result = self
            .builder
            .build_load(addr, space, strider_ir::ValueType::int_for_byte_size(out_vn.size)?)?;
        self.write_vn(out_vn, result)
    }

    pub(super) fn handle_store(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        let space = crate::lift::pcode_util::decode_space_id(insn)?;
        let addr = self.read_vn(nth_input_or_err(insn, 1)?)?;
        let data = self.read_vn(nth_input_or_err(insn, 2)?)?;
        self.builder.build_store(addr, data, space)?;
        Ok(())
    }
}
