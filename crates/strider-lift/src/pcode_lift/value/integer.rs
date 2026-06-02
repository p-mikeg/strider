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
        let value = self.read_vn(crate::pcode_lift::nth_input_or_err(insn, 0)?)?;
        let out_vn = crate::pcode_lift::require_output_vn(insn)?;
        self.write_vn(out_vn, value)
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
        let in0_size = crate::pcode_lift::nth_input_or_err(insn, 0)?.size;
        if out_vn.size < in0_size {
            return Err(anyhow::anyhow!(
                "p-code extend width mismatch: input={} output={} (output must be >= input)",
                in0_size,
                out_vn.size,
            ));
        }
        let value = self.read_vn(crate::pcode_lift::nth_input_or_err(insn, 0)?)?;
        let result = self
            .builder
            .extend_if_needed(value, strider_ir::ValueType::int_for_byte_size(out_vn.size)?, op)?;
        self.write_vn(out_vn, result)
    }
}
