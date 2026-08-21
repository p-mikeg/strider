//! The sole owner of register aliasing: overlapping sub-registers (x86-64
//! `al`/`ah`/`ax`/`eax`/`rax`, x87 ST slices, aarch64 s/d/q SIMD views) are
//! always read and written through the largest containing tracked register,
//! with shift/mask operations inserted for the slice.

use anyhow::{anyhow, bail};
use strider_ir::{ExtendOp, IRBuilderExt, IntBinaryOp, Value, ValueType, VnTypeExt};

use super::FunctionLifter;
use super::pcode_util::Result;

/// Little-endian `u64` limbs of a `width_bits`-wide value whose bits
/// `[shift, shift + set_bits)` are set.
///
/// Limb-wise so an `I256` / `I512` container mask (ymm / zmm, aarch64 SVE `z`)
/// is representable; a `u128` mask truncates those silently.
fn shifted_ones_limbs(set_bits: u64, shift: u64, width_bits: u64) -> Vec<u64> {
    let limbs = width_bits.div_ceil(64) as usize;
    let (lo, hi) = (shift, shift.saturating_add(set_bits).min(width_bits));
    (0..limbs)
        .map(|i| {
            let base = i as u64 * 64;
            let start = lo.saturating_sub(base).min(64);
            let end = hi.saturating_sub(base).min(64);
            if end > start {
                let below_end = if end == 64 {
                    u64::MAX
                } else {
                    (1u64 << end) - 1
                };
                below_end & !((1u64 << start) - 1)
            } else {
                0
            }
        })
        .collect()
}

/// Bitwise complement of `limbs` within `width_bits`.
fn complement_limbs(limbs: &[u64], width_bits: u64) -> Vec<u64> {
    limbs
        .iter()
        .enumerate()
        .map(|(i, limb)| {
            let live = (width_bits.saturating_sub(i as u64 * 64)).min(64);
            let live_mask = if live == 64 {
                u64::MAX
            } else {
                (1u64 << live) - 1
            };
            !limb & live_mask
        })
        .collect()
}

enum SubRegOutcome {
    Direct { container_reg: rsleigh::Vn },
    SubReg(SubRegContext),
}

struct SubRegContext {
    container_reg: rsleigh::Vn,
    container_ty: ValueType,
    shift_bits: u64,
}

impl<R: rsleigh::MemReader> FunctionLifter<'_, R> {
    /// Width comes from `space`'s address size; `what` names the space in the
    /// lookup-failure error.
    pub(crate) fn build_addr_const(
        &mut self,
        space: rsleigh::VnSpace,
        off: u64,
        what: &str,
    ) -> Result<strider_ir::Value> {
        let space_info = self
            .lifter
            .sleigh()
            .space_info(space)
            .ok_or_else(|| anyhow!("no space info for {what} {space:?}"))?;
        self.builder.build_int_const(
            off,
            strider_ir::ValueType::int_for_byte_size(space_info.addr_size())?,
        )
    }

    pub(crate) fn read_vns(&mut self, vns: &[rsleigh::Vn]) -> Result<Vec<strider_ir::Value>> {
        vns.iter().map(|vn| self.read_vn(vn)).collect()
    }

    pub(super) fn read_input(
        &mut self,
        insn: &rsleigh::Insn,
        n: usize,
    ) -> Result<strider_ir::Value> {
        let vn = crate::lift::pcode_util::nth_input_or_err(insn, n)?;
        self.read_vn(vn)
    }

    /// UNIQUE goes through the same aliasing path as REGISTER: Sleigh
    /// sometimes writes a wide unique and reads a narrow slice of it (MIPS
    /// MULT writes a 64-bit unique, then Copy reads 32 bits of it).
    pub(crate) fn read_vn(&mut self, vn: &rsleigh::Vn) -> Result<strider_ir::Value> {
        let space = vn.addr_space;
        match space {
            rsleigh::VnSpace::CONST => self.builder.build_int_const(vn.addr_off, vn.int_type()?),
            rsleigh::VnSpace::UNIQUE | rsleigh::VnSpace::REGISTER => self.read_reg_vn(vn),
            rsleigh::VnSpace::RAM => {
                let addr = self.build_addr_const(space, vn.addr_off, "RAM space")?;
                self.builder.build_load(addr, space, vn.int_type()?)
            }
            _ => Err(anyhow!("unsupported varnode space {space:?}")),
        }
    }

    pub(crate) fn write_vn(&mut self, vn: &rsleigh::Vn, val: strider_ir::Value) -> Result<()> {
        // Stash an ISA-mode commit for `pending_isa_mode`. Full `Vn` equality,
        // not just space+offset: a differently-sized varnode sharing the offset
        // is a different register.
        if self.isa_mode_switch_vn.is_some_and(|sw| sw == *vn)
            && let Some(addr) = self.builder.lift_addr()
        {
            // FIRST write per machine address wins. The ARMv7/v8 sla writes
            // ISAModeSwitch TWICE for `mov pc, rN`: `SetThumbMode((rN & 1) != 0)`
            // commits the real bit, then `ALUWritePC(rN & ~1)` re-derives it from
            // the already-masked value, whose cone KnownBits folds to a constant
            // 0.
            if !matches!(self.pending_isa_mode, Some((_, prev)) if prev == addr) {
                self.pending_isa_mode = Some((val, addr));
            }
        }
        let space = vn.addr_space;
        match space {
            rsleigh::VnSpace::CONST => Err(anyhow!("attempted to write to CONST space: {space:?}")),
            rsleigh::VnSpace::UNIQUE | rsleigh::VnSpace::REGISTER => self.write_reg_vn(vn, val),
            rsleigh::VnSpace::RAM => {
                let addr = self.build_addr_const(space, vn.addr_off, "RAM space")?;
                self.builder.build_store(addr, val, space)
            }
            _ => Err(anyhow!("unsupported varnode space {space:?}")),
        }
    }

    /// Largest tracked varnode in the same space that fully contains `reg`
    /// (`al` -> `rax`).  Without it a narrow read of a wide unique would
    /// return an undefined InitialVar.
    fn find_largest_fitting_register(&self, reg: &rsleigh::Vn) -> Result<rsleigh::Vn> {
        let space = reg.addr_space;
        if space != rsleigh::VnSpace::REGISTER && space != rsleigh::VnSpace::UNIQUE {
            bail!("unsupported varnode space {space:?}");
        }
        // `container_of` returns `reg` unchanged when nothing tracked contains
        // it, which here means `reg` is its own container: a legitimate
        // full-width access, not a failure.
        Ok(self.container_of(reg))
    }

    /// Bit distance between `reg`'s slot and the container's LSB.
    ///
    /// Little-endian: the LSB byte is at offset 0, so the byte distance from
    /// the container's start is the shift.  Big-endian: the MSB byte is at
    /// offset 0, so the sub-register sits `container.size - reg.size -
    /// (reg.off - container.off)` bytes above the LSB.
    ///
    /// Keyed on [`SleighArch::register_endianness`](strider_target::SleighArch::register_endianness),
    /// not the data order.
    fn calculate_reg_shift_from_container(
        &self,
        reg: &rsleigh::Vn,
        container_reg: &rsleigh::Vn,
    ) -> u64 {
        match self.lifter.arch.register_endianness() {
            strider_target::Endianness::Little => 8 * (reg.addr_off - container_reg.addr_off),
            strider_target::Endianness::Big => {
                8 * (container_reg.size as u64
                    - reg.size as u64
                    - (reg.addr_off - container_reg.addr_off))
            }
        }
    }

    /// Reads `reg` by shifting its slice out of the container and truncating.
    pub(crate) fn read_reg_vn(&mut self, reg: &rsleigh::Vn) -> Result<Value> {
        let ctx = match self.enter_sub_register(reg, "read_reg_vn")? {
            SubRegOutcome::Direct { container_reg } => {
                return self.builder.read_variable(&container_reg);
            }
            SubRegOutcome::SubReg(ctx) => ctx,
        };
        // The truncate is required even at shift 0: without it the caller gets
        // the full container width, and width-sensitive downstream ops break
        // (bitcasting an I64 read to F32 is ill-defined, so IntBitsToFloat
        // plus the optimizer drop the chain).
        let reg_ty: ValueType = reg.int_type()?;
        let curr_reg_val = self.builder.read_variable(&ctx.container_reg)?;
        let shifted = self.builder.build_shift_by_const(
            curr_reg_val,
            ctx.shift_bits,
            IntBinaryOp::ShiftRight,
            ctx.container_ty,
        )?;
        self.builder.truncate_if_needed(shifted, reg_ty)
    }

    /// Writes `val` into `reg`: read container, position `val` into reg's bit
    /// slot, clear that slot in the container, OR the two, write back.
    ///
    /// Masks are computed in *container* coordinates: reg's mask is built at
    /// `shift_bits` so it lands at reg's real position, and the preserve mask is
    /// its complement within the container.  Skipping that positioning inverts
    /// the mask on an upper-half write (AArch64 `FCVT D0,S0` zeroing the upper
    /// 64 bits of V0) and silently zeros the container's lower half.
    pub(crate) fn write_reg_vn(&mut self, reg: &rsleigh::Vn, val: Value) -> Result<()> {
        let ctx = match self.enter_sub_register(reg, "write_reg_vn")? {
            SubRegOutcome::Direct { container_reg: _ } => {
                // Register SSA slots hold integers at the register's natural
                // width, so coerce: same-width float is bit-reinterpreted, a
                // 1-bit `I1` flag result is zero-extended.  Keeping `I1` out
                // of register slots is what makes cross-region `Phi`s over a
                // register type-homogeneous.  The flag-to-`If` flow survives:
                // the cond-branch lifter narrows the read back to `I1` and
                // ConstantFold collapses the extend/truncate round trip.
                let reg_ty: ValueType = reg.int_type()?;
                let coerced = self.builder.convert_to_int_if_needed(val, reg_ty)?;
                return self.builder.write_variable(reg, coerced);
            }
            SubRegOutcome::SubReg(ctx) => ctx,
        };
        // Same coercion prelude as the direct-container arm, so both write
        // paths accept exactly the same operand types.  Going straight to
        // `build_masked_insert` would instead surface `extend_if_needed`'s
        // divergent "cannot integer-extend non-integer" for a scalar-FP write
        // into a SIMD slice.
        let reg_ty: ValueType = reg.int_type()?;
        let val = self.builder.convert_to_int_if_needed(val, reg_ty)?;

        // This write PRESERVES the bits outside `reg`, and that is correct
        // even on ISAs that zero the rest of the vector register on a
        // scalar-FP write (AArch64 `fmov s0`, x86 VEX `vmovss`) rather than
        // preserving it (legacy SSE `movss`).  Sleigh models the difference
        // itself, emitting the zeroing as separate explicit pcode ops:
        //   AArch64 `fmov s0,w0` ->  s0 = Copy(w0); reg(V0[4..]) = Copy(#0)
        //   x86 VEX  `vmovss`    ->  XMM0_Da = ...; ZMM0 = IntZext(XMM0)
        // Those arrive here as ordinary sub-register writes.
        let final_container_value = self.build_masked_insert(val, reg, &ctx)?;
        self.write_reg_vn(&ctx.container_reg, final_container_value)?;
        Ok(())
    }

    /// Shared read/write prelude.  `op` prefixes the shift-bound error so it
    /// names the originating call site.
    ///
    /// A container byte width with no `ValueType::int_for_byte_size` mapping
    /// fails here at `int_type`, before any shift or mask is built on it.
    fn enter_sub_register(&self, reg: &rsleigh::Vn, op: &'static str) -> Result<SubRegOutcome> {
        let container_reg = self.find_largest_fitting_register(reg)?;
        if container_reg == *reg {
            return Ok(SubRegOutcome::Direct { container_reg });
        }
        let container_ty: ValueType = container_reg.int_type()?;
        let shift_bits = self.calculate_reg_shift_from_container(reg, &container_reg);
        // A shift >= container bits is undefined under the IR's shift
        // semantics and would silently wrap via `shift % bit_width` on x86.
        // Containment upstream makes this unreachable (max legitimate offset
        // is `(container.size - 1) * 8`); the check turns a malformed Sleigh
        // spec into a clean lift failure instead of corrupt IR.
        if shift_bits >= (container_reg.size as u64) * 8 {
            return Err(anyhow!(
                "{op}: shift {shift_bits} >= container bit width {} \
                 (container size {} bytes); sub-register {:?} outside container {:?}",
                (container_reg.size as u64) * 8,
                container_reg.size,
                reg,
                container_reg,
            ));
        }
        Ok(SubRegOutcome::SubReg(SubRegContext {
            container_reg,
            container_ty,
            shift_bits,
        }))
    }

    /// `limbs` is little-endian.  Widths past `u128` need the limb interner.
    fn build_mask_const(&mut self, limbs: &[u64], ty: ValueType) -> Result<Value> {
        if ty.bit_width() > 128 {
            return self.builder.build_int_const_limbs(limbs, ty);
        }
        let mut bits: u128 = 0;
        for (i, limb) in limbs.iter().enumerate() {
            bits |= u128::from(*limb) << (i * 64);
        }
        self.builder.build_int_const(bits, ty)
    }

    /// Positions `val` at its bit slot in the container and merges it with the
    /// preserved container bits.
    fn build_masked_insert(
        &mut self,
        val: Value,
        reg: &rsleigh::Vn,
        ctx: &SubRegContext,
    ) -> Result<Value> {
        let ty = ctx.container_ty;
        let shift_bits = ctx.shift_bits;
        let width_bits = ty.bit_width() as u64;
        let container_mask = complement_limbs(
            &shifted_ones_limbs(u64::from(reg.size) * 8, shift_bits, width_bits),
            width_bits,
        );
        let container_val = self.builder.read_variable(&ctx.container_reg)?;

        let val_extended = self
            .builder
            .extend_if_needed(val, ty, ExtendOp::ZeroExtend)?;
        let shifted_value = self.builder.build_shift_by_const(
            val_extended,
            shift_bits,
            IntBinaryOp::ShiftLeft,
            ty,
        )?;

        let container_mask = self.build_mask_const(&container_mask, ty)?;
        let preserved = self.builder.build_int_binary_operation(
            container_val,
            container_mask,
            IntBinaryOp::And,
            ty,
        )?;

        // `shifted_value` needs no mask of its own: coercing to `reg`'s width
        // zeroed every bit above it, and the shift then landed it in exactly
        // the window `container_mask` clears.
        self.builder
            .build_int_binary_operation(preserved, shifted_value, IntBinaryOp::Or, ty)
    }
}

#[cfg(test)]
mod shift_formula_tests {
    use rsleigh::{Vn, VnSpace};

    fn reg(off: u64, size: u32) -> Vn {
        Vn {
            addr_off: off,
            addr_space: VnSpace::REGISTER,
            size,
        }
    }

    /// Little-endian shift is byte offset within the container times 8.
    #[test]
    fn le_shift_for_subregs_in_4byte_container() {
        let cases = [
            (0, 1, 0),
            (1, 1, 8),
            (2, 1, 16),
            (3, 1, 24),
            (0, 2, 0),
            (2, 2, 16),
            (0, 4, 0),
        ];
        for (sub_off, sub_size, expected) in cases {
            let container = reg(0, 4);
            let sub = reg(sub_off, sub_size);
            let shift = compute_shift_le(&sub, &container);
            assert_eq!(
                shift, expected,
                "LE sub({sub_off},{sub_size}): expected {expected}, got {shift}"
            );
        }
    }

    /// Big-endian puts the MSB at offset 0, the LSB at
    /// `container.size - sub.size`.
    #[test]
    fn be_shift_for_subregs_in_4byte_container() {
        let cases = [
            (0, 1, 24),
            (1, 1, 16),
            (2, 1, 8),
            (3, 1, 0),
            (0, 2, 16),
            (2, 2, 0),
            (0, 4, 0),
        ];
        for (sub_off, sub_size, expected) in cases {
            let container = reg(0, 4);
            let sub = reg(sub_off, sub_size);
            let shift = compute_shift_be(&sub, &container);
            assert_eq!(
                shift, expected,
                "BE sub({sub_off},{sub_size}): expected {expected}, got {shift}"
            );
        }
    }

    #[test]
    fn be_shift_for_subregs_in_8byte_container() {
        let container = reg(0, 8);
        assert_eq!(compute_shift_be(&reg(0, 4), &container), 32);
        assert_eq!(compute_shift_be(&reg(4, 4), &container), 0);
        assert_eq!(compute_shift_be(&reg(0, 1), &container), 56);
        assert_eq!(compute_shift_be(&reg(7, 1), &container), 0);
    }

    // Mirror calculate_reg_shift_from_container's two arms so the formulas are
    // testable without spinning up a lifter.
    fn compute_shift_le(reg: &Vn, container: &Vn) -> u64 {
        8 * (reg.addr_off - container.addr_off)
    }
    fn compute_shift_be(reg: &Vn, container: &Vn) -> u64 {
        8 * (container.size as u64 - reg.size as u64 - (reg.addr_off - container.addr_off))
    }
}

#[cfg(test)]
mod mask_limb_tests {
    use super::{complement_limbs, shifted_ones_limbs};

    fn as_u128(limbs: &[u64]) -> u128 {
        assert!(limbs.len() <= 2, "as_u128 on a >128-bit mask");
        limbs
            .iter()
            .enumerate()
            .fold(0u128, |acc, (i, l)| acc | (u128::from(*l) << (i * 64)))
    }

    /// The positioned mask selects exactly the bits reg occupies.
    #[test]
    fn positioned_mask_isolates_reg_bits_inside_16byte_container() {
        let q0 = shifted_ones_limbs(128, 0, 128);
        assert_eq!(as_u128(&q0), u128::MAX, "container mask is all-ones");
        assert_eq!(
            as_u128(&shifted_ones_limbs(32, 0, 128)),
            0xFFFF_FFFF,
            "s0 occupies bits 0..32"
        );
        let d0 = shifted_ones_limbs(64, 0, 128);
        assert_eq!(
            as_u128(&d0),
            0xFFFF_FFFF_FFFF_FFFF,
            "d0 occupies bits 0..64"
        );
        // The AArch64 SIMD upper-half hot spot.
        let upper8 = shifted_ones_limbs(64, 64, 128);
        assert_eq!(
            as_u128(&upper8),
            0xFFFF_FFFF_FFFF_FFFF_0000_0000_0000_0000,
            "the 8-byte sub at offset 8 occupies bits 64..128"
        );
        assert_eq!(as_u128(&d0) & as_u128(&upper8), 0, "disjoint");
        assert_eq!(as_u128(&d0) | as_u128(&upper8), as_u128(&q0), "covering");
    }

    /// The preserve mask keeps exactly the bits that don't belong to reg.
    #[test]
    fn container_mask_for_upper_half_write_keeps_lower_half() {
        let container_mask = complement_limbs(&shifted_ones_limbs(64, 64, 128), 128);
        assert_eq!(
            as_u128(&container_mask),
            0x0000_0000_0000_0000_FFFF_FFFF_FFFF_FFFF,
            "an upper-8 write preserves the lower 8 bytes of q0"
        );
    }

    /// Same, for the shift-0 lower-half write.
    #[test]
    fn container_mask_for_lower_half_write_keeps_upper_half() {
        let container_mask = complement_limbs(&shifted_ones_limbs(64, 0, 128), 128);
        assert_eq!(
            as_u128(&container_mask),
            0xFFFF_FFFF_FFFF_FFFF_0000_0000_0000_0000,
            "a d0 (lower 8) write preserves the upper 8 bytes of q0"
        );
    }

    /// Pins the invariant that makes preserve-then-let-Sleigh-zero exact: the
    /// scalar write's positioned mask and the zero-writes' positioned masks
    /// are disjoint and together tile the container, so after all of them the
    /// container is fully determined with no stale bits and no gaps.
    #[test]
    fn aarch64_scalar_fp_write_then_upper_zero_tiles_the_container() {
        // Sleigh lifts `fmov s0,w0` to `s0 = Copy(w0)` plus aligned zero-writes
        // over the rest: observed as 4 bytes at offset 4 and 8 at offset 8.
        let s0 = as_u128(&shifted_ones_limbs(32, 0, 128));
        let mid = as_u128(&shifted_ones_limbs(32, 32, 128));
        let hi = as_u128(&shifted_ones_limbs(64, 64, 128));
        assert_eq!(s0 & mid, 0);
        assert_eq!(s0 & hi, 0);
        assert_eq!(mid & hi, 0);
        assert_eq!(
            s0 | mid | hi,
            as_u128(&shifted_ones_limbs(128, 0, 128)),
            "s0 and the zero-fills tile the full V0 container"
        );
    }

    /// Exercises every shift count the LE formula produces.
    #[test]
    fn container_mask_byte_subregs_in_4byte_container() {
        let cases: [(u64, u32); 4] = [
            (0, 0xFFFF_FF00),
            (8, 0xFFFF_00FF),
            (16, 0xFF00_FFFF),
            (24, 0x00FF_FFFF),
        ];
        for (shift, expected) in cases {
            let mask = complement_limbs(&shifted_ones_limbs(8, shift, 32), 32);
            assert_eq!(
                as_u128(&mask),
                u128::from(expected),
                "byte-sub at shift {shift}: expected container_mask 0x{expected:08x}"
            );
        }
    }

    /// Every container width the sub-register path builds masks for.
    #[test]
    fn mask_covers_only_the_declared_width() {
        for bytes in [1u64, 2, 4, 6, 8, 10, 12, 14, 16] {
            let bits = bytes * 8;
            let mask = as_u128(&shifted_ones_limbs(bits, 0, 128));
            let want = if bits == 128 {
                u128::MAX
            } else {
                (1u128 << bits) - 1
            };
            assert_eq!(mask, want, "{bytes}-byte mask is the low {bits} bits");
        }
    }

    /// ymm / zmm / SVE `z`: the widths a `u128` mask could not hold.
    #[test]
    fn wide_container_masks_are_representable_as_limbs() {
        let ymm_low8 = shifted_ones_limbs(64, 0, 256);
        assert_eq!(ymm_low8, vec![u64::MAX, 0, 0, 0]);
        assert_eq!(
            complement_limbs(&ymm_low8, 256),
            vec![0, u64::MAX, u64::MAX, u64::MAX],
        );
        let zmm_mid32 = shifted_ones_limbs(256, 128, 512);
        assert_eq!(
            zmm_mid32,
            vec![0, 0, u64::MAX, u64::MAX, u64::MAX, u64::MAX, 0, 0],
        );
        assert_eq!(
            complement_limbs(&shifted_ones_limbs(512, 0, 512), 512),
            vec![0; 8],
            "the full-width mask complements to zero"
        );
    }
}
