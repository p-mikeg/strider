use strider_ir::{ExtendOp, IRBuilderExt, VnTypeExt};

use crate::lift::FunctionLifter;
use crate::lift::pcode_util::{Result, nth_input_or_err, require_output_vn};

impl<'a, R: rsleigh::MemReader> FunctionLifter<'a, R> {
    /// Sleigh contracts equal input and output sizes (`OpBehaviorCopy::
    /// evaluateUnary` returns `in1` unchanged), so a mismatch is a malformed
    /// decode.  Unchecked, `write_vn`'s `convert_to_int_if_needed` would
    /// truncate a wider operand and zero-extend a narrower one, the second
    /// being wrong for a signed carrier.
    pub(super) fn handle_copy(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        let out_vn = require_output_vn(insn)?;
        super::arithmetic::require_equal_input_output_width(nth_input_or_err(insn, 0)?, out_vn)?;
        let value = self.read_input(insn, 0)?;
        self.write_vn(out_vn, value)
    }

    /// Sleigh contracts `output.size >= input.size`; this check names the
    /// `.sla` bug when that is violated.
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
