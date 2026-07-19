//! Bit-positioning and slicing opcodes: `Subpiece`, `Popcount`, `Lzcount`,
//! `Piece`, `Extract`, `Insert`.

use strider_ir::node::ValueType;
use strider_ir::{IRBuilderExt, IntBinaryOp, VnTypeExt};

use anyhow::bail;

use crate::lift::FunctionLifter;
use crate::lift::pcode_util::{Result, ensure_const_space, nth_input_or_err, require_output_vn};

/// Checked, not `as u8`: a value above 255 from a malformed `.sla` would wrap
/// silently.
fn extract_bit_pos_u8(
    vn: &rsleigh::Vn,
    opcode: rsleigh::Opcode,
    label: &'static str,
) -> Result<u8> {
    u8::try_from(vn.addr_off).map_err(|_| {
        anyhow::anyhow!(
            "{label} {} does not fit in u8 (opcode {:?})",
            vn.addr_off,
            opcode,
        )
    })
}

/// `(1 << len) - 1`, saturating at `u128::MAX`.  Computed in `u128` so an I128
/// or I80 field is covered exactly.
fn low_bits_mask(len: u8) -> u128 {
    if (len as usize) >= 128 {
        u128::MAX
    } else {
        (1u128 << len) - 1
    }
}

/// `Or(And(dest, !mask_shifted), ShiftLeft(And(src, mask_raw), lsb))`.
fn build_bit_field_insert(
    builder: &mut strider_ir::FunctionBuilder,
    dest: strider_ir::Value,
    src: strider_ir::Value,
    lsb: u8,
    len: u8,
    out_ty: ValueType,
) -> Result<strider_ir::Value> {
    // u128 so an I128 / I80 `out_ty` with `lsb + len > 64` gets correct bits
    // in slots 64..127.
    let mask_raw: u128 = low_bits_mask(len);
    let mask_shifted = mask_raw.wrapping_shl(lsb as u32);
    let not_mask_shifted = !mask_shifted;

    let not_m_const = builder.build_int_const(not_mask_shifted, out_ty)?;
    let cleared =
        builder.build_int_binary_operation(dest, not_m_const, IntBinaryOp::And, out_ty)?;

    let mask_const = builder.build_int_const(mask_raw, out_ty)?;
    let src_masked =
        builder.build_int_binary_operation(src, mask_const, IntBinaryOp::And, out_ty)?;

    let src_positioned =
        builder.build_shift_by_const(src_masked, u64::from(lsb), IntBinaryOp::ShiftLeft, out_ty)?;

    builder.build_int_binary_operation(cleared, src_positioned, IntBinaryOp::Or, out_ty)
}

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
        // Guards a future Subpiece-width extension; the check above already
        // pins bit_shift to at most 120, under the u128 IR width.
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
    /// lowerings, which require matching widths; `popcount` / `lzcount`
    /// legitimately narrow a wider input, so they leave it off.
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
        let value = self.builder.convert_to_int_if_needed(value, out_ty)?;
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
        // Shift by the low piece's PHYSICAL bit width from the varnode byte
        // size, not the SSA type's.  They differ only for `I1`, a 1-bit boolean
        // in a 1-byte flag register whose `bit_width()` is 1; the varnode size
        // keeps Piece faithful to the pcode geometry there.
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

    pub(super) fn handle_extract(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        // inputs are (value, lsb, bit_count); lowers to
        // `Truncate(ShiftRight(x, lsb), narrow_ty)`, plus an And mask when
        // `len < narrow_ty.bit_width()` to preserve "upper bits zero".
        ensure_const_space(nth_input_or_err(insn, 1)?, insn.opcode, "Extract lsb")?;
        ensure_const_space(nth_input_or_err(insn, 2)?, insn.opcode, "Extract bit_count")?;
        let input_vn = nth_input_or_err(insn, 0)?;
        let value = self.read_vn(input_vn)?;
        let lsb = extract_bit_pos_u8(nth_input_or_err(insn, 1)?, insn.opcode, "Extract lsb")?;
        let len = extract_bit_pos_u8(nth_input_or_err(insn, 2)?, insn.opcode, "Extract bit_count")?;
        let out_vn = require_output_vn(insn)?;
        let narrow_ty: ValueType = out_vn.int_type()?;
        // Physical int width from the varnode byte size, not the SSA type; see
        // the `I1` note in `handle_piece`.  The bounds check and shift/mask
        // below must operate on the real bit geometry.
        let x_nat_ty = input_vn.int_type()?;
        // Slice must lie within the input width: past it, Sleigh clamps to zero
        // while the host mask wraps mod width, so the two disagree.
        let in_bits = x_nat_ty.bit_width();
        if lsb as usize + len as usize > in_bits {
            bail!(
                "Extract slice [lsb={lsb}, len={len}] exceeds input width {in_bits} bits (opcode {:?})",
                insn.opcode
            );
        }
        let x_int = self.builder.convert_to_int_if_needed(value, x_nat_ty)?;
        let shifted = self.builder.build_shift_by_const(
            x_int,
            u64::from(lsb),
            IntBinaryOp::ShiftRight,
            x_nat_ty,
        )?;
        let narrowed = self.builder.truncate_if_needed(shifted, narrow_ty)?;
        let result = if (len as usize) < narrow_ty.bit_width() {
            // u128 mask: a u64 one would cap at 0xFFFF_FFFF_FFFF_FFFF and, once
            // zero-extended by `build_int_const`, would wrongly zero bits
            // 64..127 for an I128 `narrow_ty` with `len >= 64`.
            let mask = self
                .builder
                .build_int_const(low_bits_mask(len), narrow_ty)?;
            self.builder
                .build_int_binary_operation(narrowed, mask, IntBinaryOp::And, narrow_ty)?
        } else {
            narrowed
        };
        self.write_vn(out_vn, result)
    }

    pub(super) fn handle_insert(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        // inputs are (dest, src, lsb, bit_count).
        ensure_const_space(nth_input_or_err(insn, 2)?, insn.opcode, "Insert lsb")?;
        ensure_const_space(nth_input_or_err(insn, 3)?, insn.opcode, "Insert bit_count")?;
        let dest = self.read_input(insn, 0)?;
        let src = self.read_input(insn, 1)?;
        let lsb = extract_bit_pos_u8(nth_input_or_err(insn, 2)?, insn.opcode, "Insert lsb")?;
        let len = extract_bit_pos_u8(nth_input_or_err(insn, 3)?, insn.opcode, "Insert bit_count")?;
        let out_vn = require_output_vn(insn)?;
        let out_ty: ValueType = out_vn.int_type()?;
        // Field must fit the destination: past the width the host
        // `wrapping_shl` mask and the width-clamped IR `ShiftLeft` disagree.
        let out_bits = out_ty.bit_width();
        if lsb as usize + len as usize > out_bits {
            bail!(
                "Insert field [lsb={lsb}, len={len}] exceeds destination width {out_bits} bits (opcode {:?})",
                insn.opcode
            );
        }

        let dest_wide = self.builder.convert_to_int_if_needed(dest, out_ty)?;
        let src_wide = self.builder.convert_to_int_if_needed(src, out_ty)?;

        let result =
            build_bit_field_insert(&mut self.builder, dest_wide, src_wide, lsb, len, out_ty)?;
        self.write_vn(out_vn, result)
    }
}
