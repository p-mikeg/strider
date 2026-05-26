//! Memory-load opcode (`Load`).
//!
//! `Store` lives in strider — it advances the memory chain in a way the
//! value lifter doesn't model.

use crate::pcode_lift::ValueLifter;
use crate::pcode_lift::Result;

impl<'a, R: rsleigh::MemReader> ValueLifter<'a, R> {
    pub(super) fn handle_load(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        let space = crate::pcode_lift::decode_space_id(insn)?;
        let addr = self.read_vn(crate::pcode_lift::nth_input_or_err(insn, 1)?)?;
        let out_vn = crate::pcode_lift::require_output_vn(insn)?;
        let out = self
            .builder
            .build_load(addr, space, out_vn.size.try_into()?)?;
        self.write_vn(out_vn, out)
    }
}
