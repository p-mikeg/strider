use strider_ir::{ExtendOp, IRBuilderExt, IntBinaryOp, VnTypeExt};

use anyhow::bail;

use crate::FunctionLifter;
use crate::pcode_util::{Result, ensure_const_space, nth_input_or_err, require_output_vn};

impl<'a, R: rsleigh::MemReader> FunctionLifter<'a, R> {
    /// Sleigh contracts equal input and output sizes (`OpBehaviorCopy::
    /// evaluateUnary` returns `in1` unchanged), so a mismatch is a malformed
    /// decode.  Unchecked, `write_vn`'s `convert_to_int_if_needed` would
    /// truncate a wider operand and zero-extend a narrower one, the second
    /// being wrong for a signed carrier.
    pub(super) fn handle_copy(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        let out_vn = require_output_vn(insn)?;
        super::arithmetic::require_same_size(nth_input_or_err(insn, 0)?, out_vn, "unary op")?;
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

    /// Right-shift by `byte_offset * 8`, then coerce to the output width.
    ///
    /// Sleigh's `truncation` operator keeps the offset inside the input, so an
    /// offset at or past the input width means the decode is not what this
    /// handler models: fail the lift rather than answer it.
    /// `OpBehaviorSubpiece::evaluateBinary` folds an offset to zero only at or
    /// past the 8-byte host word (`opbehavior.cc:759-766`), which is its
    /// emulator's limit on a wider input, not a rule about the input width.
    ///
    /// An output WIDER than the input is accepted and zero-extends, which is
    /// `& calc_mask(sizeout)` over the narrower value (`opbehavior.cc:764`).
    pub(super) fn handle_subpiece(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        let input_vn = nth_input_or_err(insn, 0)?;
        let offset_vn = nth_input_or_err(insn, 1)?;
        ensure_const_space(offset_vn, insn.opcode, "Subpiece byte-offset")?;
        let byte_offset = offset_vn.addr_off;
        if byte_offset >= u64::from(input_vn.size) {
            bail!(
                "Subpiece byte_offset {byte_offset} out of range for input size {} (opcode {:?})",
                input_vn.size,
                insn.opcode
            );
        }
        let value = self.read_vn(input_vn)?;
        let out_vn = require_output_vn(insn)?;
        // No overflow: byte_offset < input.size <= u32::MAX.
        let bit_shift = byte_offset * 8;
        let shifted = self.builder.build_shift_by_const(
            value,
            bit_shift,
            IntBinaryOp::ShiftRight,
            input_vn.int_type()?,
        )?;
        let result = self
            .builder
            .convert_to_int_if_needed(shifted, out_vn.int_type()?)?;
        self.write_vn(out_vn, result)
    }

    pub(super) fn handle_piece(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        // inputs are (hi, lo); lowers to
        // `Or(ShiftLeft(ZeroExtend(hi), lo_bits), ZeroExtend(lo))`.
        let hi_vn = nth_input_or_err(insn, 0)?;
        let lo_vn = nth_input_or_err(insn, 1)?;
        let out_vn = require_output_vn(insn)?;
        // Sleigh contracts `hi.size + lo.size == out.size`.  An unbalanced
        // Piece would silently drop or duplicate bits under this lowering.
        let pieces_sum = u64::from(hi_vn.size) + u64::from(lo_vn.size);
        if pieces_sum != u64::from(out_vn.size) {
            bail!(
                "Piece size invariant: hi.size ({}) + lo.size ({}) = {} \
                 must equal out.size ({}); opcode {:?}",
                hi_vn.size,
                lo_vn.size,
                pieces_sum,
                out_vn.size,
                insn.opcode,
            );
        }
        let hi = self.read_vn(hi_vn)?;
        let lo = self.read_vn(lo_vn)?;
        let out_ty = out_vn.int_type()?;
        let hi_wide = self.builder.convert_to_int_if_needed(hi, out_ty)?;
        let lo_wide = self.builder.convert_to_int_if_needed(lo, out_ty)?;
        // The low piece's PHYSICAL bit width, from the varnode byte size: the
        // `hi.size + lo.size == out.size` check above is what makes shifting
        // the high piece by exactly this much lossless.
        let lo_bits = u64::from(lo_vn.size) * 8;
        let hi_shifted =
            self.builder
                .build_shift_by_const(hi_wide, lo_bits, IntBinaryOp::ShiftLeft, out_ty)?;
        let result = self.builder.build_int_binary_operation(
            hi_shifted,
            lo_wide,
            IntBinaryOp::Or,
            out_ty,
        )?;
        self.write_vn(out_vn, result)
    }
}
