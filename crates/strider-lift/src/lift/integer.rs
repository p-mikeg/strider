//! Integer value-producing pcode opcodes that are NOT arithmetic:
//! `Copy`, `IntZext`, `IntSext`.
//!
//! Pure integer arithmetic (`IntAdd`, `IntSub`, `IntMul`, …) lives in
//! [`super::arithmetic`].  Slice/extract/insert/popcount/etc. live in
//! [`super::cast`].

use strider_ir::{ExtendOp, IRBuilderExt, VnTypeExt};

use crate::lift::FunctionLifter;
use crate::lift::pcode_util::{Result, nth_input_or_err, require_output_vn};

impl<'a, R: rsleigh::MemReader> FunctionLifter<'a, R> {
    /// Translates a p-code `Copy` instruction.
    pub(super) fn handle_copy(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        let value = self.read_input(insn, 0)?;
        let out_vn = require_output_vn(insn)?;
        self.write_vn(out_vn, value)
    }

    /// Translates a p-code zero-extend or sign-extend instruction into an IR
    /// extend node and writes the result to the output varnode.
    ///
    /// Sleigh's `IntZext` / `IntSext` contract requires `output.size >=
    /// input.size`.  `extend_if_needed` now rejects a wider-than-target input
    /// outright, but check here first to surface the `.sla` inversion as a
    /// clearer, extend-specific lift-time error.
    pub(super) fn process_extend(&mut self, insn: &rsleigh::Insn, op: ExtendOp) -> Result<()> {
        let out_vn = require_output_vn(insn)?;
        let in0_size = nth_input_or_err(insn, 0)?.size;
        if out_vn.size < in0_size {
            return Err(anyhow::anyhow!(
                "p-code extend width mismatch: input={} output={} (output must be >= input)",
                in0_size,
                out_vn.size,
            ));
        }
        let value = self.read_input(insn, 0)?;
        let result = self
            .builder
            .extend_if_needed(value, out_vn.int_type()?, op)?;
        self.write_vn(out_vn, result)
    }
}
