//! Integer value-producing pcode opcodes that are NOT arithmetic:
//! `Copy`, `IntZext`, `IntSext`.
//!
//! Pure integer arithmetic (`IntAdd`, `IntSub`, `IntMul`, …) lives in
//! [`super::arithmetic`].  Slice/extract/insert/popcount/etc. live in
//! [`super::cast`].

use strider_ir::ExtendOp;

use crate::pcode_lift::Result;
use crate::pcode_lift::ValueLifter;

impl<'a, R: rsleigh::MemReader> ValueLifter<'a, R> {
    /// Translates a p-code `Copy` instruction.
    pub(super) fn handle_copy(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        let input = self.read_vn(&insn.inputs[0])?;
        let out_vn = crate::pcode_lift::require_output_vn(insn)?;
        self.write_vn(out_vn, input)
    }

    /// Translates a p-code zero-extend or sign-extend instruction into an IR
    /// extend node and writes the result to the output varnode.
    pub(super) fn process_extend(&mut self, insn: &rsleigh::Insn, op: ExtendOp) -> Result<()> {
        let input = self.read_vn(&insn.inputs[0])?;
        let out_vn = crate::pcode_lift::require_output_vn(insn)?;
        let out = self
            .builder
            .extend_if_needed(input, out_vn.size.try_into()?, op)?;
        self.write_vn(out_vn, out)
    }
}
