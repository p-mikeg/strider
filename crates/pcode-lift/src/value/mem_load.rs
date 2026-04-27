//! Memory-load opcode (`Load`).
//!
//! `Store` lives in strider — it advances the memory chain in a way the
//! value lifter doesn't model.

use crate::error::{ErrorKind, Result};
use crate::ValueLifter;

/// Decodes the target address space of a p-code `LOAD`.
///
/// P-code encodes the target space as a CONST-space varnode at `inputs[0]`
/// whose offset is a pointer to the Sleigh `AddrSpace` object.  Reading
/// `.addr.space` directly yields `CONST` (the space of that encoding
/// varnode), not the actual target space — callers that care about the
/// target must decode via [`rsleigh::VnSpace::by_id`].
fn decode_space_id(insn: &rsleigh::Insn) -> Result<rsleigh::VnSpace> {
    let space_id_vn = *insn
        .inputs
        .first()
        .ok_or(ErrorKind::TooFewInputs(insn.opcode, 1, 0))?;
    if space_id_vn.addr.space != rsleigh::VnSpace::CONST {
        return Err(ErrorKind::ExpectedConstInput(insn.opcode, 0).into());
    }
    // SAFETY: `space_id_vn` is the `inputs[0]` of a LOAD p-code insn and
    // was just verified to live in CONST space, which is the precondition
    // of `VnSpace::by_id`.
    Ok(unsafe { rsleigh::VnSpace::by_id(space_id_vn) })
}

impl<'a, R: rsleigh::MemReader> ValueLifter<'a, R> {
    pub(super) fn handle_load(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        let space = decode_space_id(insn)?;
        let addr = self.read_vn(&insn.inputs[1])?;
        let out_vn = crate::require_output_vn(insn)?;
        let out = self
            .builder
            .build_load(addr, space, out_vn.size.try_into()?)?;
        self.write_vn(out_vn, out)
    }
}
