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
    ///
    /// Sleigh's `IntZext` / `IntSext` contract requires `output.size >=
    /// input.size`.  A malformed `.sla` emitting `output.size <
    /// input.size` would silently invoke `extend_if_needed`'s truncate
    /// path — surface the inversion as a lift-time error.
    pub(super) fn process_extend(&mut self, insn: &rsleigh::Insn, op: ExtendOp) -> Result<()> {
        let out_vn = crate::pcode_lift::require_output_vn(insn)?;
        if out_vn.size < insn.inputs[0].size {
            return Err(anyhow::anyhow!(
                "p-code extend width mismatch: input={} output={} (output must be >= input)",
                insn.inputs[0].size,
                out_vn.size,
            ));
        }
        let input = self.read_vn(&insn.inputs[0])?;
        let out = self
            .builder
            .extend_if_needed(input, out_vn.size.try_into()?, op)?;
        self.write_vn(out_vn, out)
    }
}
