//! The sole owner of register aliasing: overlapping sub-registers (x86-64
//! `al`/`ah`/`ax`/`eax`/`rax`, x87 ST slices, aarch64 s/d/q SIMD views) are
//! always read and written through the largest containing tracked register,
//! with shift/mask operations inserted for the slice.

use anyhow::{anyhow, bail};
use strider_ir::{ExtendOp, IRBuilderExt, IntBinaryOp, Value, ValueType, VnTypeExt};

use super::FunctionLifter;
use super::pcode_util::Result;

/// Bitmask covering a varnode's width, used to merge a sub-register with the
/// surrounding bits of its container.
///
/// 10 bytes is x87 ST0/STn extended precision.  32/64 bytes (ymm/zmm) fail
/// closed: a 256-/512-bit mask has no `u128` representation, and returning a
/// truncated `u128::MAX` would be silently wrong.  Those widths are still
/// valid *containers*: full-width access takes the direct path (no mask), a
/// sub-register read slices via shift+truncate (no mask), and a sub-register
/// write is rejected in `write_reg_vn`.  So this arm is unreachable in
/// production; it exists so a wrong mask is impossible by construction rather
/// than only by that guard.
fn vn_mask(reg: &rsleigh::Vn) -> Result<u128> {
    match reg.size {
        1 => Ok(u128::from(u8::MAX)),
        2 => Ok(u128::from(u16::MAX)),
        4 => Ok(u128::from(u32::MAX)),
        8 => Ok(u128::from(u64::MAX)),
        10 => Ok((1u128 << 80) - 1),
        16 => Ok(u128::MAX),
        32 | 64 => Err(anyhow!(
            "register size {} bytes (>16) has no representable u128 mask; \
             wide ymm/zmm registers are accessed full-width via the direct \
             container path, never by masking",
            reg.size,
        )),
        _ => Err(anyhow!("unsupported register size {} bytes", reg.size)),
    }
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
    fn calculate_reg_shift_from_container(
        &self,
        reg: &rsleigh::Vn,
        container_reg: &rsleigh::Vn,
    ) -> u64 {
        match self.lifter.arch.endianness() {
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
    /// Masks are computed in *container* coordinates: reg's `vn_mask` is
    /// always in the low-bits domain, so it must be shifted by `shift_bits`
    /// to land at reg's real position, and the preserve mask is its
    /// complement.  Skipping that positioning inverts the mask on an
    /// upper-half write (AArch64 `FCVT D0,S0` zeroing the upper 64 bits of
    /// V0) and silently zeros the container's lower half.
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
        // A sub-register write into a >16-byte ymm/zmm container would need a
        // container-coordinate mask that `u128` cannot hold.  Fail closed.
        // The read path needs no mask, so it has no such guard.
        if ctx.container_reg.size > 16 {
            return Err(anyhow!(
                "write_reg_vn: sub-register write within a wide ({}-byte) \
                 container is not supported (a >16-byte mask has no u128 \
                 representation) (reg {:?}, container {:?})",
                ctx.container_reg.size,
                reg,
                ctx.container_reg,
            ));
        }
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
        // Those arrive here as ordinary sub-register writes, so preserving
        // within the scalar op is right precisely because the zeroing is its
        // own op.  No per-arch policy needed.
        let final_container_value = self.build_masked_insert(val, reg, &ctx)?;
        self.write_reg_vn(&ctx.container_reg, final_container_value)?;
        Ok(())
    }

    /// Shared read/write prelude.  `op` prefixes the shift-bound error so it
    /// names the originating call site.
    ///
    /// Wide (>16-byte) containers are deliberately NOT rejected here: reads
    /// slice them via shift+truncate on the container's `I256`/`I512` type and
    /// need no mask.  Only the write path needs the guard, and it lives in
    /// `write_reg_vn`.
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
        let reg_mask = vn_mask(reg)? << shift_bits;
        let container_mask = vn_mask(&ctx.container_reg)? & !reg_mask;
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

        let reg_val =
            self.builder
                .build_const_binop(reg_mask, shifted_value, IntBinaryOp::And, ty)?;
        let preserved =
            self.builder
                .build_const_binop(container_mask, container_val, IntBinaryOp::And, ty)?;

        self.builder
            .build_int_binary_operation(preserved, reg_val, IntBinaryOp::Or, ty)
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

// Regression: the write mask must select reg's position INSIDE the container,
// not reg's bits in the low-bytes domain.  Using `vn_mask(reg)` unshifted
// happens to work at shift 0 but at shift > 0 (upper 8 bytes of a 16-byte SIMD
// register, written when AArch64 FCVT D0,S0 zeroes the upper half of V0) it
// inverts container_mask and zeros the lower half, orphaning the FCVT chain
// through ConstantFold's `(a&C1 | b&C2) & C3` and `x & 0 -> 0` rules.

#[cfg(test)]
mod positioned_mask_tests {
    use super::vn_mask;
    use rsleigh::{Vn, VnSpace};

    fn reg_at(off: u64, size: u32) -> Vn {
        Vn {
            addr_off: off,
            addr_space: VnSpace::REGISTER,
            size,
        }
    }

    /// `vn_mask(reg) << shift_bits` must select exactly the bits reg occupies.
    #[test]
    fn positioned_mask_isolates_reg_bits_inside_16byte_container() {
        let q0 = reg_at(0, 16);
        let s0 = reg_at(0, 4); // lower 4 bytes
        let d0 = reg_at(0, 8); // lower 8 bytes
        let v0_upper8 = reg_at(8, 8); // the AArch64 SIMD upper-half hot spot

        let q0_mask = vn_mask(&q0).unwrap();
        assert_eq!(q0_mask, u128::MAX, "container mask should be all-ones");

        let s0_pos = vn_mask(&s0).unwrap(); // shift 0
        assert_eq!(s0_pos, 0xFFFF_FFFF, "s0 occupies bits 0..32");

        let d0_pos = vn_mask(&d0).unwrap();
        assert_eq!(d0_pos, 0xFFFF_FFFF_FFFF_FFFF, "d0 occupies bits 0..64");

        let upper8_pos = vn_mask(&v0_upper8).unwrap() << 64;
        assert_eq!(
            upper8_pos, 0xFFFF_FFFF_FFFF_FFFF_0000_0000_0000_0000,
            "upper 8-byte sub at offset 8 occupies bits 64..128"
        );

        assert_eq!(d0_pos & upper8_pos, 0, "d0 and upper-half are disjoint");
        assert_eq!(d0_pos | upper8_pos, q0_mask, "d0 ∪ upper-half = full q0");
    }

    /// The preserve mask must keep exactly the bits that don't belong to reg.
    #[test]
    fn container_mask_for_upper_half_write_keeps_lower_half() {
        let q0 = reg_at(0, 16);
        let upper8 = reg_at(8, 8);

        let positioned_reg_mask = vn_mask(&upper8).unwrap() << 64;
        let container_mask = vn_mask(&q0).unwrap() & !positioned_reg_mask;

        assert_eq!(
            container_mask, 0x0000_0000_0000_0000_FFFF_FFFF_FFFF_FFFF,
            "upper-8 write must preserve the lower 8 bytes of q0"
        );
        assert_ne!(
            container_mask, 0xFFFF_FFFF_FFFF_FFFF_0000_0000_0000_0000,
            "regression check: container_mask must NOT be the upper-half mask"
        );
    }

    /// Same, for the shift-0 lower-half write.
    #[test]
    fn container_mask_for_lower_half_write_keeps_upper_half() {
        let q0 = reg_at(0, 16);
        let d0 = reg_at(0, 8);

        let positioned_reg_mask = vn_mask(&d0).unwrap();
        let container_mask = vn_mask(&q0).unwrap() & !positioned_reg_mask;

        assert_eq!(
            container_mask, 0xFFFF_FFFF_FFFF_FFFF_0000_0000_0000_0000,
            "d0 (lower 8) write must preserve the upper 8 bytes of q0"
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
        let v0 = reg_at(0, 16);
        let s0 = reg_at(0, 4);
        let zero_mid = reg_at(4, 4);
        let zero_hi = reg_at(8, 8);

        let s0_pos = vn_mask(&s0).unwrap(); // shift 0
        let mid_pos = vn_mask(&zero_mid).unwrap() << 32;
        let hi_pos = vn_mask(&zero_hi).unwrap() << 64;

        // Disjoint: no write clobbers another.
        assert_eq!(s0_pos & mid_pos, 0);
        assert_eq!(s0_pos & hi_pos, 0);
        assert_eq!(mid_pos & hi_pos, 0);
        // Covering: low 4 bytes = value, upper 12 = 0, nothing stale.
        assert_eq!(
            s0_pos | mid_pos | hi_pos,
            vn_mask(&v0).unwrap(),
            "s0 ∪ zero-fills must tile the full V0 container"
        );
    }

    /// Exercises every shift count the LE formula produces.
    #[test]
    fn container_mask_byte_subregs_in_4byte_container() {
        let container = reg_at(0, 4);
        let cases: [(u64, u32, u32); 4] = [
            (0, 0, 0xFFFF_FF00),
            (1, 8, 0xFFFF_00FF),
            (2, 16, 0xFF00_FFFF),
            (3, 24, 0x00FF_FFFF),
        ];
        for (off, shift, expected_container_mask) in cases {
            let sub = reg_at(off, 1);
            let positioned = vn_mask(&sub).unwrap() << shift;
            let mask = vn_mask(&container).unwrap() & !positioned;
            assert_eq!(
                mask & 0xFFFF_FFFF,
                u128::from(expected_container_mask),
                "byte-sub at off {off}, shift {shift}: \
                 expected container_mask 0x{expected_container_mask:08x}, got 0x{mask:032x}"
            );
        }
    }
}

#[cfg(test)]
mod vn_mask_tests {
    use super::*;
    use rsleigh::{Vn, VnSpace};

    fn reg(size: u32) -> Vn {
        Vn {
            size,
            addr_off: 0,
            addr_space: VnSpace::REGISTER,
        }
    }

    #[test]
    fn mask_covers_only_the_declared_width() -> Result<()> {
        assert_eq!(vn_mask(&reg(1))?, u128::from(u8::MAX));
        assert_eq!(vn_mask(&reg(2))?, u128::from(u16::MAX));
        assert_eq!(vn_mask(&reg(4))?, u128::from(u32::MAX));
        assert_eq!(vn_mask(&reg(8))?, u128::from(u64::MAX));
        assert_eq!(vn_mask(&reg(10))?, (1u128 << 80) - 1);
        assert_eq!(vn_mask(&reg(16))?, u128::MAX);
        Ok(())
    }

    #[test]
    fn vn_mask_for_10_bytes_is_low_80_bits() -> Result<()> {
        let mask = vn_mask(&reg(10))?;
        assert_eq!(mask & ((1u128 << 80) - 1), (1u128 << 80) - 1);
        assert_eq!(mask >> 80, 0);
        assert_eq!(mask, 0x_0000_FFFF_FFFF_FFFF_FFFF_FFFFu128);
        Ok(())
    }

    #[test]
    fn narrower_mask_is_subset_of_wider_mask() -> Result<()> {
        let m1 = vn_mask(&reg(1))?;
        let m2 = vn_mask(&reg(2))?;
        let m4 = vn_mask(&reg(4))?;
        let m8 = vn_mask(&reg(8))?;
        let m16 = vn_mask(&reg(16))?;
        assert_eq!(m1 & m2, m1);
        assert_eq!(m2 & m4, m2);
        assert_eq!(m4 & m8, m4);
        assert_eq!(m8 & m16, m8);
        Ok(())
    }

    #[test]
    fn unsupported_sizes_return_unsupported_reg_size_error() {
        for &bad in &[0u32, 3, 5, 6, 7, 9, 17, 33, 65, u32::MAX] {
            let r = vn_mask(&reg(bad));
            match r {
                Err(e) => assert!(
                    e.to_string()
                        .contains(&format!("unsupported register size {bad}")),
                    "size {bad}: expected UnsupportedRegSize, got {e}"
                ),
                Ok(_) => panic!("size {bad}: expected error, got Ok"),
            }
        }
    }
}

#[cfg(test)]
mod wide_register_tests {
    use super::vn_mask;
    use rsleigh::{Vn, VnSpace};

    fn reg(off: u64, size: u32) -> Vn {
        Vn {
            addr_space: VnSpace::REGISTER,
            addr_off: off,
            size,
        }
    }

    // ymm/zmm fail closed rather than returning a truncated `u128::MAX`.  They
    // are still valid containers: full-width access takes the direct path, a
    // sub-register read slices without a mask, and a sub-register write is
    // rejected in `write_reg_vn`.
    #[test]
    fn vn_mask_rejects_32_bytes_ymm_no_representable_mask() {
        let ymm = reg(0x1000, 32);
        assert!(
            vn_mask(&ymm).is_err(),
            "a 256-bit ymm mask cannot be represented in u128 — must fail closed"
        );
    }

    #[test]
    fn vn_mask_rejects_64_bytes_zmm_no_representable_mask() {
        let zmm = reg(0x1000, 64);
        assert!(
            vn_mask(&zmm).is_err(),
            "a 512-bit zmm mask cannot be represented in u128 — must fail closed"
        );
    }

    #[test]
    fn vn_mask_still_rejects_unsupported_widths() {
        assert!(vn_mask(&reg(0, 3)).is_err());
        assert!(vn_mask(&reg(0, 7)).is_err());
        assert!(vn_mask(&reg(0, 128)).is_err());
    }
}
