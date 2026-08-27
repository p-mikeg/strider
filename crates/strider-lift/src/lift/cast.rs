use strider_ir::node::ValueType;
use strider_ir::{IRBuilderExt, IntBinaryOp, VnTypeExt};

use anyhow::bail;

use crate::lift::FunctionLifter;
use crate::lift::pcode_util::{Result, ensure_const_space, nth_input_or_err, require_output_vn};

impl<'a, R: rsleigh::MemReader> FunctionLifter<'a, R> {
    /// Right-shift by `byte_offset * 8`, then truncate.  P-code requires
    /// `byte_offset < value_size`; a larger one wraps the multiply, so it is
    /// rejected outright.
    pub(super) fn handle_subpiece(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        let input_vn = nth_input_or_err(insn, 0)?;
        ensure_const_space(
            nth_input_or_err(insn, 1)?,
            insn.opcode,
            "Subpiece byte-offset",
        )?;
        let byte_offset = nth_input_or_err(insn, 1)?.addr_off;
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
        // Guards a future Subpiece-width extension. The check above bounds
        // `bit_shift` by the input's own width, which reaches 504 on a `zmm`.
        debug_assert!(
            bit_shift < u64::from(input_vn.size) * 8,
            "Subpiece bit_shift {bit_shift} must be < input bit-width {}",
            u64::from(input_vn.size) * 8,
        );
        let shifted = self.builder.build_shift_by_const(
            value,
            bit_shift,
            IntBinaryOp::ShiftRight,
            input_vn.int_type()?,
        )?;
        let result = self
            .builder
            .truncate_if_needed(shifted, out_vn.int_type()?)?;
        self.write_vn(out_vn, result)
    }

    /// Shared envelope for single-input integer unary ops.
    ///
    /// `enforce_equal_io_width` is on for the `Int2Comp` / `IntNeg`-as-xor
    /// lowerings, which require matching widths, and gates the input coercion
    /// with it: `popcount` / `lzcount` count over the INPUT's width, so
    /// widening their operand would shift the result by the width difference.
    pub(super) fn lift_int_unary(
        &mut self,
        insn: &rsleigh::Insn,
        enforce_equal_io_width: bool,
        build: impl FnOnce(
            &mut strider_ir::FunctionBuilder,
            strider_ir::Value,
            ValueType,
        ) -> Result<strider_ir::Value>,
    ) -> Result<()> {
        let out_vn = require_output_vn(insn)?;
        if enforce_equal_io_width {
            super::arithmetic::require_equal_input_output_width(
                nth_input_or_err(insn, 0)?,
                out_vn,
            )?;
        }
        let value = self.read_input(insn, 0)?;
        let out_ty = out_vn.int_type()?;
        let value = if enforce_equal_io_width {
            self.builder.convert_to_int_if_needed(value, out_ty)?
        } else {
            value
        };
        let result = build(&mut self.builder, value, out_ty)?;
        self.write_vn(out_vn, result)
    }

    pub(super) fn handle_popcount(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        self.lift_int_unary(insn, false, |b, v, t| b.build_popcount(v, t))
    }

    pub(super) fn handle_lzcount(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        self.lift_int_unary(insn, false, |b, v, t| b.build_lzcount(v, t))
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
        let out_ty: ValueType = out_vn.int_type()?;
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
