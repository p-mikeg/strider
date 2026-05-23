//! Bit-positioning, slicing, and pointer-arithmetic opcodes:
//! `Subpiece`, `Popcount`, `Lzcount`, `Piece`, `Extract`, `Insert`,
//! `PtrAdd`, `PtrSub`, and the no-op `Cast`.

use strider_ir::node::NodeOutputType;
use strider_ir::IntBinaryOp;

use anyhow::bail;

use crate::pcode_lift::Result;
use crate::pcode_lift::ValueLifter;

/// Asserts that a varnode `vn` lives in CONST space.  Sleigh encodes the
/// "this is a literal constant value" varnode by setting `addr_space ==
/// CONST` with the constant in `addr_off`.  Several opcode handlers
/// (Subpiece's `byte_offset`, Extract/Insert's `lsb`/`bit_count`,
/// PtrAdd's `elem_size`) read `vn.addr_off` directly as a literal value
/// and would silently mis-decode any non-CONST input.  This is a
/// defensive structural guard: GHIDRA's Sleigh emitter always produces
/// CONST in these slots, but a malformed `.sla` spec or a fuzzer-built
/// `Insn` would otherwise produce a structurally valid but semantically
/// wrong IR shape.
fn ensure_const_space(
    vn: &rsleigh::Vn,
    opcode: rsleigh::Opcode,
    slot_label: &str,
) -> Result<()> {
    if vn.addr_space != rsleigh::VnSpace::CONST {
        bail!(
            "opcode {opcode:?}: {slot_label} must be a CONST-space varnode \
             (got addr_space {:?}); Sleigh's contract requires this slot \
             to encode a literal value",
            vn.addr_space,
        );
    }
    Ok(())
}

impl<'a, R: rsleigh::MemReader> ValueLifter<'a, R> {
    /// Translates a no-op `Cast` instruction.
    ///
    /// GHIDRA docs: "semantically equivalent to a COPY operation".
    pub(super) fn handle_cast(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        let input = self.read_vn(&insn.inputs[0])?;
        let out_vn = crate::pcode_lift::require_output_vn(insn)?;
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
        ensure_const_space(&insn.inputs[1], insn.opcode, "Subpiece byte-offset")?;
        let byte_offset = insn.inputs[1].addr_off;
        if byte_offset >= u64::from(input_vn.size) {
            bail!(
                "Subpiece byte_offset {byte_offset} out of range for input size {} (opcode {:?})",
                input_vn.size, insn.opcode
            );
        }
        let input = self.read_vn(input_vn)?;
        let out_vn = crate::pcode_lift::require_output_vn(insn)?;
        let shifted = if byte_offset == 0 {
            input
        } else {
            // safe: byte_offset < input.size <= u32::MAX, so byte_offset * 8 fits in u64
            let bit_shift = byte_offset * 8;
            // Defensive guard against future Subpiece-width extensions: the
            // upstream `byte_offset < input_vn.size` check already pins
            // `bit_shift <= (input_vn.size - 1) * 8` which today caps at
            // 120 < 128 (max supported `u128` IR width).  If a future
            // Subpiece variant ever widened past `u128` inputs, the shift
            // would silently exceed the IR's representable bit-width.
            debug_assert!(
                bit_shift < u64::from(input_vn.size) * 8,
                "Subpiece bit_shift {bit_shift} must be < input bit-width {}",
                u64::from(input_vn.size) * 8,
            );
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
        let out_vn = crate::pcode_lift::require_output_vn(insn)?;
        let out = self
            .builder
            .build_popcount(input, out_vn.size.try_into()?)?;
        self.write_vn(out_vn, out)
    }

    pub(super) fn handle_lzcount(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        let input = self.read_vn(&insn.inputs[0])?;
        let out_vn = crate::pcode_lift::require_output_vn(insn)?;
        let out = self.builder.build_lzcount(input, out_vn.size.try_into()?)?;
        self.write_vn(out_vn, out)
    }

    pub(super) fn handle_piece(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        // inputs[0] = hi (most significant), inputs[1] = lo (least significant).
        // Lowered to: Or(ShiftLeft(ZeroExtend(hi), lo_bits), ZeroExtend(lo)).
        let hi_vn = &insn.inputs[0];
        let lo_vn = &insn.inputs[1];
        let out_vn = crate::pcode_lift::require_output_vn(insn)?;
        // Sleigh's Piece contract: `hi.size + lo.size == out.size`.  A
        // malformed spec emitting an unbalanced Piece would silently drop
        // or duplicate bits since the lowering uses `hi.shift_by(lo.bits)`
        // and OR-merges with a zero-extended `lo`.
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
        ensure_const_space(&insn.inputs[1], insn.opcode, "Extract lsb")?;
        ensure_const_space(&insn.inputs[2], insn.opcode, "Extract bit_count")?;
        let input = self.read_vn(&insn.inputs[0])?;
        // Bit-position constants live in `addr_off: u64`; the lowering
        // (`lsb_const`, mask `(1u128 << len) - 1`) caps both at 128.
        // A spec emitting a value > 255 would have silently wrapped the
        // older `as u8` cast — surface it as a lift-time error.
        let lsb = u8::try_from(insn.inputs[1].addr_off).map_err(|_| {
            anyhow::anyhow!(
                "Extract lsb {} does not fit in u8 (opcode {:?})",
                insn.inputs[1].addr_off,
                insn.opcode,
            )
        })?;
        let len = u8::try_from(insn.inputs[2].addr_off).map_err(|_| {
            anyhow::anyhow!(
                "Extract bit_count {} does not fit in u8 (opcode {:?})",
                insn.inputs[2].addr_off,
                insn.opcode,
            )
        })?;
        let out_vn = crate::pcode_lift::require_output_vn(insn)?;
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
            // Compute the AND-mask in u128 so a U128 narrow_ty with
            // 64 ≤ len < 128 produces a mask covering the requested
            // upper bits.  Using u64 here would cap the mask at
            // 0xFFFF_FFFF_FFFF_FFFF, then `build_int_const` would
            // zero-extend to u128 and the result would zero bits
            // 64..127 of the narrowed value.
            let mask_val: u128 = if (len as usize) >= 128 {
                u128::MAX
            } else {
                (1u128 << len) - 1
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
        ensure_const_space(&insn.inputs[2], insn.opcode, "Insert lsb")?;
        ensure_const_space(&insn.inputs[3], insn.opcode, "Insert bit_count")?;
        let dest = self.read_vn(&insn.inputs[0])?;
        let src = self.read_vn(&insn.inputs[1])?;
        // Same u64→u8 narrowing rationale as `handle_extract`.
        let lsb = u8::try_from(insn.inputs[2].addr_off).map_err(|_| {
            anyhow::anyhow!(
                "Insert lsb {} does not fit in u8 (opcode {:?})",
                insn.inputs[2].addr_off,
                insn.opcode,
            )
        })?;
        let len = u8::try_from(insn.inputs[3].addr_off).map_err(|_| {
            anyhow::anyhow!(
                "Insert bit_count {} does not fit in u8 (opcode {:?})",
                insn.inputs[3].addr_off,
                insn.opcode,
            )
        })?;
        let out_vn = crate::pcode_lift::require_output_vn(insn)?;
        let out_ty: NodeOutputType = out_vn.size.try_into()?;

        let dest_ty = self.builder.get_output_type(dest)?.to_natural_int_type();
        let dest_int = self.builder.convert_to_int_if_needed(dest, dest_ty)?;
        let src_ty = self.builder.get_output_type(src)?.to_natural_int_type();
        let src_int = self.builder.convert_to_int_if_needed(src, src_ty)?;

        let dest_wide = self.builder.convert_to_int_if_needed(dest_int, out_ty)?;
        let src_wide = self.builder.convert_to_int_if_needed(src_int, out_ty)?;

        // Compute masks in u128 so a U128 (or U80) `out_ty` with
        // `lsb + len > 64` produces correct bits in slots 64..127.
        // Using u64 here would silently zero those slots through
        // `build_int_const`'s u64→u128 zero-extension.
        let mask_raw: u128 = if (len as usize) >= 128 {
            u128::MAX
        } else {
            (1u128 << len) - 1
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
        ensure_const_space(&insn.inputs[2], insn.opcode, "PtrAdd elem_size")?;
        let base = self.read_vn(&insn.inputs[0])?;
        let index = self.read_vn(&insn.inputs[1])?;
        let elem_size = insn.inputs[2].addr_off;
        let out_vn = crate::pcode_lift::require_output_vn(insn)?;
        let out_ty: strider_ir::ValueType = out_vn.size.try_into()?;
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

    /// `PtrSub(base, index)` lowers to `Add(base, Neg(index))` via the
    /// same canonicalisation that `IntSub` uses.  See
    /// [`super::arithmetic::ValueLifter::handle_int_sub`] for the
    /// rationale behind avoiding `IntBinaryOp::Sub`.
    pub(super) fn handle_ptr_sub(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        let base = self.read_vn(&insn.inputs[0])?;
        let index = self.read_vn(&insn.inputs[1])?;
        let out_vn = crate::pcode_lift::require_output_vn(insn)?;
        let out_ty = out_vn.size.try_into()?;
        let neg_index = self.builder.build_int_unary_operation(
            index,
            strider_ir::IntUnaryOp::Neg,
            out_ty,
        )?;
        let out = self.builder.build_int_binary_operation(
            base,
            neg_index,
            IntBinaryOp::Add,
            out_ty,
        )?;
        self.write_vn(out_vn, out)
    }
}
