//! Bit-positioning, slicing, and pointer-arithmetic opcodes:
//! `Subpiece`, `Popcount`, `Lzcount`, `Piece`, `Extract`, `Insert`,
//! `PtrAdd`, `PtrSub`, and the no-op `Cast`.

use ir::node::NodeOutputType;
use ir::IntBinaryOp;

use anyhow::bail;

use crate::Result;
use crate::ValueLifter;

impl<'a, R: rsleigh::MemReader> ValueLifter<'a, R> {
    /// Translates a no-op `Cast` instruction.
    ///
    /// GHIDRA docs: "semantically equivalent to a COPY operation".
    pub(super) fn handle_cast(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        let input = self.read_vn(&insn.inputs[0])?;
        let out_vn = crate::require_output_vn(insn)?;
        self.write_vn(out_vn, input)
    }

    /// `Subpiece(value, byte_offset, out_size)`: extracts `out_size` bytes
    /// starting at byte `byte_offset` from `value`.
    ///
    /// Implemented as: right-shift by `byte_offset * 8` bits, then truncate.
    /// P-code Subpiece's contract requires `byte_offset < value_size`; any
    /// larger value would wrap on the multiply or produce a useless shift,
    /// so we reject it explicitly.
    pub(super) fn handle_subpiece(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        let input_vn = &insn.inputs[0];
        let byte_offset = insn.inputs[1].addr.off;
        if byte_offset >= u64::from(input_vn.size) {
            bail!(
                "Subpiece byte_offset {byte_offset} out of range for input size {} (opcode {:?})",
                input_vn.size, insn.opcode
            );
        }
        let input = self.read_vn(input_vn)?;
        let out_vn = crate::require_output_vn(insn)?;
        let shifted = if byte_offset == 0 {
            input
        } else {
            // safe: byte_offset < input.size <= u32::MAX, so byte_offset * 8 fits in u64
            let bit_shift = byte_offset * 8;
            let shift_const = self
                .builder
                .build_int_const(bit_shift, input_vn.size.try_into()?)?;
            self.builder.build_int_binary_operation(
                input,
                shift_const,
                IntBinaryOp::ShiftRight,
                input_vn.size.try_into()?,
            )?
        };
        let out = self
            .builder
            .truncate_if_needed(shifted, out_vn.size.try_into()?)?;
        self.write_vn(out_vn, out)
    }

    pub(super) fn handle_popcount(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        let input = self.read_vn(&insn.inputs[0])?;
        let out_vn = crate::require_output_vn(insn)?;
        let out = self
            .builder
            .build_popcount(input, out_vn.size.try_into()?)?;
        self.write_vn(out_vn, out)
    }

    pub(super) fn handle_lzcount(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        let input = self.read_vn(&insn.inputs[0])?;
        let out_vn = crate::require_output_vn(insn)?;
        let out = self.builder.build_lzcount(input, out_vn.size.try_into()?)?;
        self.write_vn(out_vn, out)
    }

    pub(super) fn handle_piece(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        // inputs[0] = hi (most significant), inputs[1] = lo (least significant).
        // Lowered to: Or(ShiftLeft(ZeroExtend(hi), lo_bits), ZeroExtend(lo)).
        let hi = self.read_vn(&insn.inputs[0])?;
        let lo = self.read_vn(&insn.inputs[1])?;
        let out_vn = crate::require_output_vn(insn)?;
        let out_ty: NodeOutputType = out_vn.size.try_into()?;
        let hi_ty = self.builder.get_output_type(hi)?.to_natural_int_type();
        let hi_int = self.builder.convert_to_int_if_needed(hi, hi_ty)?;
        let lo_ty = self.builder.get_output_type(lo)?.to_natural_int_type();
        let lo_int = self.builder.convert_to_int_if_needed(lo, lo_ty)?;
        let lo_bits = lo_ty.bit_width() as u64;
        let hi_wide = self.builder.convert_to_int_if_needed(hi_int, out_ty)?;
        let lo_wide = self.builder.convert_to_int_if_needed(lo_int, out_ty)?;
        let shift_amt = self.builder.build_int_const(lo_bits, out_ty)?;
        let hi_shifted = self.builder.build_int_binary_operation(
            hi_wide,
            shift_amt,
            IntBinaryOp::ShiftLeft,
            out_ty,
        )?;
        let out = self.builder.build_int_binary_operation(
            hi_shifted,
            lo_wide,
            IntBinaryOp::Or,
            out_ty,
        )?;
        self.write_vn(out_vn, out)
    }

    pub(super) fn handle_extract(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        // inputs[0] = value, inputs[1] = lsb (CONST), inputs[2] = bit_count (CONST)
        // Lowered to: Truncate(ShiftRight(x, lsb), narrow_ty), with an extra
        // And mask when len < narrow_ty.bit_width() to preserve "upper bits zero".
        let input = self.read_vn(&insn.inputs[0])?;
        let lsb = insn.inputs[1].addr.off as u8;
        let len = insn.inputs[2].addr.off as u8;
        let out_vn = crate::require_output_vn(insn)?;
        let narrow_ty: NodeOutputType = out_vn.size.try_into()?;
        let x_nat_ty = self.builder.get_output_type(input)?.to_natural_int_type();
        let x_int = self.builder.convert_to_int_if_needed(input, x_nat_ty)?;
        let shifted = if lsb == 0 {
            x_int
        } else {
            let lsb_const = self.builder.build_int_const(lsb as u64, x_nat_ty)?;
            self.builder.build_int_binary_operation(
                x_int,
                lsb_const,
                IntBinaryOp::ShiftRight,
                x_nat_ty,
            )?
        };
        let narrowed = self.builder.truncate_if_needed(shifted, narrow_ty)?;
        let out = if (len as usize) < narrow_ty.bit_width() {
            let mask_val = if len >= 64 {
                u64::MAX
            } else {
                (1u64 << len) - 1
            };
            let mask = self.builder.build_int_const(mask_val, narrow_ty)?;
            self.builder.build_int_binary_operation(
                narrowed,
                mask,
                IntBinaryOp::And,
                narrow_ty,
            )?
        } else {
            narrowed
        };
        self.write_vn(out_vn, out)
    }

    pub(super) fn handle_insert(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        // inputs[0] = dest, inputs[1] = src, inputs[2] = lsb (CONST), inputs[3] = bit_count (CONST).
        // Lowered to: Or(And(dest, !mask_shifted), ShiftLeft(And(src, mask_raw), lsb)).
        let dest = self.read_vn(&insn.inputs[0])?;
        let src = self.read_vn(&insn.inputs[1])?;
        let lsb = insn.inputs[2].addr.off as u8;
        let len = insn.inputs[3].addr.off as u8;
        let out_vn = crate::require_output_vn(insn)?;
        let out_ty: NodeOutputType = out_vn.size.try_into()?;

        let dest_ty = self.builder.get_output_type(dest)?.to_natural_int_type();
        let dest_int = self.builder.convert_to_int_if_needed(dest, dest_ty)?;
        let src_ty = self.builder.get_output_type(src)?.to_natural_int_type();
        let src_int = self.builder.convert_to_int_if_needed(src, src_ty)?;

        let dest_wide = self.builder.convert_to_int_if_needed(dest_int, out_ty)?;
        let src_wide = self.builder.convert_to_int_if_needed(src_int, out_ty)?;

        let mask_raw = if len >= 64 {
            u64::MAX
        } else {
            (1u64 << len) - 1
        };
        let mask_shifted = mask_raw.wrapping_shl(lsb as u32);
        let not_mask_shifted = !mask_shifted;

        let not_m_const = self.builder.build_int_const(not_mask_shifted, out_ty)?;
        let cleared = self.builder.build_int_binary_operation(
            dest_wide,
            not_m_const,
            IntBinaryOp::And,
            out_ty,
        )?;

        let mask_const = self.builder.build_int_const(mask_raw, out_ty)?;
        let src_masked = self.builder.build_int_binary_operation(
            src_wide,
            mask_const,
            IntBinaryOp::And,
            out_ty,
        )?;

        let src_positioned = if lsb == 0 {
            src_masked
        } else {
            let lsb_const = self.builder.build_int_const(lsb as u64, out_ty)?;
            self.builder.build_int_binary_operation(
                src_masked,
                lsb_const,
                IntBinaryOp::ShiftLeft,
                out_ty,
            )?
        };

        let out = self.builder.build_int_binary_operation(
            cleared,
            src_positioned,
            IntBinaryOp::Or,
            out_ty,
        )?;
        self.write_vn(out_vn, out)
    }

    pub(super) fn handle_ptr_add(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        let base = self.read_vn(&insn.inputs[0])?;
        let index = self.read_vn(&insn.inputs[1])?;
        let elem_size = insn.inputs[2].addr.off;
        let out_vn = crate::require_output_vn(insn)?;
        let out_ty: ir::ValueType = out_vn.size.try_into()?;
        let elem_const = self.builder.build_int_const(elem_size, out_ty)?;
        let scaled = self.builder.build_int_binary_operation(
            index,
            elem_const,
            IntBinaryOp::Mul,
            out_ty,
        )?;
        let out = self.builder.build_int_binary_operation(
            base,
            scaled,
            IntBinaryOp::Add,
            out_ty,
        )?;
        self.write_vn(out_vn, out)
    }

    pub(super) fn handle_ptr_sub(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        let base = self.read_vn(&insn.inputs[0])?;
        let index = self.read_vn(&insn.inputs[1])?;
        let out_vn = crate::require_output_vn(insn)?;
        let out = self.builder.build_int_binary_operation(
            base,
            index,
            IntBinaryOp::Sub,
            out_vn.size.try_into()?,
        )?;
        self.write_vn(out_vn, out)
    }
}
