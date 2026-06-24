//! Bit-positioning, slicing, and pointer-arithmetic opcodes:
//! `Subpiece`, `Popcount`, `Lzcount`, `Piece`, `Extract`, `Insert`,
//! `PtrAdd`, `PtrSub`, and the no-op `Cast`.

use strider_ir::IRBuilderExt;
use strider_ir::IRViewer;
use strider_ir::IntBinaryOp;
use strider_ir::VnTypeExt;
use strider_ir::node::ValueType;

use anyhow::bail;

use crate::lift::FunctionLifter;
use crate::lift::pcode_util::{Result, ensure_const_space, nth_input_or_err, require_output_vn};

/// Reads a bit-position constant from `vn.addr_off` and narrows it to `u8`.
///
/// Both [`FunctionLifter::handle_extract`] and [`FunctionLifter::handle_insert`]
/// read `lsb` and `bit_count` from CONST-space varnodes this way.
/// A value > 255 would have silently wrapped the older `as u8` cast; surfacing
/// it as a typed error enables accurate diagnostics for malformed `.sla` specs.
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

/// Constructs the bit-field-insert IR: `Or(And(dest, !mask_shifted), ShiftLeft(And(src, mask_raw), lsb))`.
///
/// Extracted from [`FunctionLifter::handle_insert`] to isolate the mask-and-position
/// IR construction from the input-preparation steps.
fn build_bit_field_insert(
    builder: &mut strider_ir::FunctionBuilder,
    dest: strider_ir::Value,
    src: strider_ir::Value,
    lsb: u8,
    len: u8,
    out_ty: ValueType,
) -> Result<strider_ir::Value> {
    // Compute masks in u128 so a I128 (or I80) `out_ty` with
    // `lsb + len > 64` produces correct bits in slots 64..127.
    let mask_raw: u128 = if (len as usize) >= 128 {
        u128::MAX
    } else {
        (1u128 << len) - 1
    };
    let mask_shifted = mask_raw.wrapping_shl(lsb as u32);
    let not_mask_shifted = !mask_shifted;

    let not_m_const = builder.build_int_const(not_mask_shifted, out_ty)?;
    let cleared =
        builder.build_int_binary_operation(dest, not_m_const, IntBinaryOp::And, out_ty)?;

    let mask_const = builder.build_int_const(mask_raw, out_ty)?;
    let src_masked =
        builder.build_int_binary_operation(src, mask_const, IntBinaryOp::And, out_ty)?;

    let src_positioned = if lsb == 0 {
        src_masked
    } else {
        let lsb_const = builder.build_int_const(lsb as u64, out_ty)?;
        builder.build_int_binary_operation(src_masked, lsb_const, IntBinaryOp::ShiftLeft, out_ty)?
    };

    builder.build_int_binary_operation(cleared, src_positioned, IntBinaryOp::Or, out_ty)
}

impl<'a, R: rsleigh::MemReader> FunctionLifter<'a, R> {
    /// `Subpiece(value, byte_offset, out_size)`: extracts `out_size` bytes
    /// starting at byte `byte_offset` from `value`.
    ///
    /// Implemented as: right-shift by `byte_offset * 8` bits, then truncate.
    /// P-code Subpiece's contract requires `byte_offset < value_size`; any
    /// larger value would wrap on the multiply or produce a useless shift,
    /// so we reject it explicitly.
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
        let shifted = if byte_offset == 0 {
            value
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
            let input_ty = input_vn.int_type()?;
            // The shift constant carries the *input* width.  `build_int_const`
            // masks the (small) shift amount to `input_ty` — for the wide
            // I256 / I512 case the mask is `u128::MAX`, so the interned node is
            // byte-identical to the explicit-limb path.
            let shift_const = self.builder.build_int_const(u128::from(bit_shift), input_ty)?;
            self.builder.build_int_binary_operation(
                value,
                shift_const,
                IntBinaryOp::ShiftRight,
                input_ty,
            )?
        };
        let result = self.builder.truncate_if_needed(shifted, out_vn.int_type()?)?;
        self.write_vn(out_vn, result)
    }

    /// Shared envelope for single-input integer unary ops: read input 0,
    /// coerce it to the output width, run `build`, and write the result.
    /// Covers the character-identical `popcount` / `lzcount` handlers.
    fn lift_int_unary(
        &mut self,
        insn: &rsleigh::Insn,
        build: impl FnOnce(
            &mut strider_ir::FunctionBuilder,
            strider_ir::Value,
            ValueType,
        ) -> Result<strider_ir::Value>,
    ) -> Result<()> {
        let value = self.read_input(insn, 0)?;
        let out_vn = require_output_vn(insn)?;
        let out_ty = out_vn.int_type()?;
        let value = self.builder.convert_to_int_if_needed(value, out_ty)?;
        let result = build(&mut self.builder, value, out_ty)?;
        self.write_vn(out_vn, result)
    }

    pub(super) fn handle_popcount(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        self.lift_int_unary(insn, |b, v, t| b.build_popcount(v, t))
    }

    pub(super) fn handle_lzcount(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        self.lift_int_unary(insn, |b, v, t| b.build_lzcount(v, t))
    }

    pub(super) fn handle_piece(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        // inputs[0] = hi (most significant), inputs[1] = lo (least significant).
        // Lowered to: Or(ShiftLeft(ZeroExtend(hi), lo_bits), ZeroExtend(lo)).
        let hi_vn = nth_input_or_err(insn, 0)?;
        let lo_vn = nth_input_or_err(insn, 1)?;
        let out_vn = require_output_vn(insn)?;
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
        let out_ty: ValueType = out_vn.int_type()?;
        // Convert `hi` / `lo` straight to the output width.  Register reads
        // are already integer-typed and `convert_to_int_if_needed` handles
        // the width directly, so the prior natural-width intermediate
        // conversion was a redundant round-trip.
        let hi_wide = self.builder.convert_to_int_if_needed(hi, out_ty)?;
        let lo_wide = self.builder.convert_to_int_if_needed(lo, out_ty)?;
        // Shift `hi` by the *physical* bit-width of the low piece, derived from
        // the varnode byte size — not from the SSA value's type.  These agree
        // for every type except `I1` (a 1-bit boolean stored as-is in a 1-byte
        // flag register, whose `bit_width()` is 1, not 8); using the varnode
        // size keeps Piece faithful to the pcode geometry in that case.
        let lo_bits = u64::from(lo_vn.size) * 8;
        let shift_amt = self.builder.build_int_const(lo_bits, out_ty)?;
        let hi_shifted = self.builder.build_int_binary_operation(
            hi_wide,
            shift_amt,
            IntBinaryOp::ShiftLeft,
            out_ty,
        )?;
        let result = self.builder.build_int_binary_operation(
            hi_shifted,
            lo_wide,
            IntBinaryOp::Or,
            out_ty,
        )?;
        self.write_vn(out_vn, result)
    }

    pub(super) fn handle_extract(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        // inputs[0] = value, inputs[1] = lsb (CONST), inputs[2] = bit_count (CONST)
        // Lowered to: Truncate(ShiftRight(x, lsb), narrow_ty), with an extra
        // And mask when len < narrow_ty.bit_width() to preserve "upper bits zero".
        ensure_const_space(nth_input_or_err(insn, 1)?, insn.opcode, "Extract lsb")?;
        ensure_const_space(nth_input_or_err(insn, 2)?, insn.opcode, "Extract bit_count")?;
        let input_vn = nth_input_or_err(insn, 0)?;
        let value = self.read_vn(input_vn)?;
        let lsb = extract_bit_pos_u8(nth_input_or_err(insn, 1)?, insn.opcode, "Extract lsb")?;
        let len = extract_bit_pos_u8(nth_input_or_err(insn, 2)?, insn.opcode, "Extract bit_count")?;
        let out_vn = require_output_vn(insn)?;
        let narrow_ty: ValueType = out_vn.int_type()?;
        // Work in the input's *physical* int width (from its varnode byte size),
        // not the SSA value's natural type.  They agree for every type except
        // `I1` (a 1-bit boolean held in a 1-byte flag register, whose
        // `bit_width()` is 1, not 8); using the varnode size makes the slice
        // bounds-check and the shift/mask operate on the real bit geometry.
        let x_nat_ty = input_vn.int_type()?;
        // The extracted slice [lsb, lsb+len) must lie within the input width;
        // shifting past the width yields width-clamped (Sleigh) zero in the IR
        // but the host mask uses mod-width wrapping, so reject the mismatch.
        let in_bits = x_nat_ty.bit_width();
        if lsb as usize + len as usize > in_bits {
            bail!(
                "Extract slice [lsb={lsb}, len={len}] exceeds input width {in_bits} bits (opcode {:?})",
                insn.opcode
            );
        }
        let x_int = self.builder.convert_to_int_if_needed(value, x_nat_ty)?;
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
        let result = if (len as usize) < narrow_ty.bit_width() {
            // Compute the AND-mask in u128 so a I128 narrow_ty with
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
            self.builder
                .build_int_binary_operation(narrowed, mask, IntBinaryOp::And, narrow_ty)?
        } else {
            narrowed
        };
        self.write_vn(out_vn, result)
    }

    pub(super) fn handle_insert(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        // inputs[0] = dest, inputs[1] = src, inputs[2] = lsb (CONST), inputs[3] = bit_count (CONST).
        // Lowered to: Or(And(dest, !mask_shifted), ShiftLeft(And(src, mask_raw), lsb)).
        ensure_const_space(nth_input_or_err(insn, 2)?, insn.opcode, "Insert lsb")?;
        ensure_const_space(nth_input_or_err(insn, 3)?, insn.opcode, "Insert bit_count")?;
        let dest = self.read_input(insn, 0)?;
        let src = self.read_input(insn, 1)?;
        let lsb = extract_bit_pos_u8(nth_input_or_err(insn, 2)?, insn.opcode, "Insert lsb")?;
        let len = extract_bit_pos_u8(nth_input_or_err(insn, 3)?, insn.opcode, "Insert bit_count")?;
        let out_vn = require_output_vn(insn)?;
        let out_ty: ValueType = out_vn.int_type()?;
        // The inserted field [lsb, lsb+len) must fit in the destination.  Past
        // the width the host-side `wrapping_shl` mask and the IR `ShiftLeft`
        // (width-clamped) disagree, so reject rather than emit wrong bits.
        let out_bits = out_ty.bit_width();
        if lsb as usize + len as usize > out_bits {
            bail!(
                "Insert field [lsb={lsb}, len={len}] exceeds destination width {out_bits} bits (opcode {:?})",
                insn.opcode
            );
        }

        let dest_ty = self.builder.value_type(dest)?.to_natural_int_type();
        let dest_int = self.builder.convert_to_int_if_needed(dest, dest_ty)?;
        let src_ty = self.builder.value_type(src)?.to_natural_int_type();
        let src_int = self.builder.convert_to_int_if_needed(src, src_ty)?;

        let dest_wide = self.builder.convert_to_int_if_needed(dest_int, out_ty)?;
        let src_wide = self.builder.convert_to_int_if_needed(src_int, out_ty)?;

        let result =
            build_bit_field_insert(&mut self.builder, dest_wide, src_wide, lsb, len, out_ty)?;
        self.write_vn(out_vn, result)
    }
}
