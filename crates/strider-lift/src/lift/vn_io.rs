//! Varnode read/write dispatch for the per-CFG lifter.
//!
//! Translates a [`rsleigh::Vn`] (Sleigh's location descriptor — register,
//! unique temp, constant, or memory address) into the IR primitives the
//! caller needs.  This module OWNS the machine-specific register-aliasing
//! logic: overlapping sub-registers (`al`/`ah`/`ax`/`eax`/`rax` on x86-64,
//! low-byte slices of x87 ST registers, the s/d/q SIMD views on aarch64,
//! etc.) are handled by always reading and writing through the largest
//! containing tracked register and inserting bit-shift / mask operations for
//! sub-register slices.  strider-ir itself treats the varnodes it is given as
//! an opaque universe; the lifter is the sole owner of sub-register slicing.

use anyhow::{anyhow, bail};
use strider_ir::{ExtendOp, IRBuilderExt, IntBinaryOp, Value, ValueType, VnTypeExt};

use super::FunctionLifter;
use super::pcode_util::Result;

/// Returns a bitmask that covers all bits for a varnode's width in bytes.
///
/// Used when reading or writing a sub-register inside a larger container
/// register — the mask selects only the bits belonging to the sub-register
/// so they can be merged with the surrounding bits of the container.
///
/// Supported sizes:
/// * 1, 2, 4, 8 bytes — standard integer-register widths.
/// * 10 bytes — x87 ST0/STn 80-bit FPU stack registers.  Models the
///   80-bit extended-precision width via `(1u128 << 80) - 1`.
/// * 16 bytes — wider sub-register writes through 16-byte SIMD container
///   registers (XMM0 on x86_64, q0 on aarch64).
/// * 32 / 64 bytes — AVX-2 `ymm` / AVX-512 `zmm` registers: **fail
///   closed**.  A 256-/512-bit mask has no `u128` representation, so
///   `vn_mask` returns an error rather than a silently-truncated
///   `u128::MAX`.  These widths are still valid *containers*; a full-width
///   access takes the direct container read/write path which never consults
///   the mask (`read_vn`/`write_vn` early-out when `reg` equals its own
///   container), and a sub-register slice *within* a >16-byte container is
///   rejected in `enter_sub_register` before any mask is computed.  So
///   `vn_mask` is only ever called for ≤16-byte registers in production, and
///   erroring here makes a wrong degraded mask impossible by construction
///   instead of relying solely on that guard.
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

/// Outcome of the shared sub-register entry check — either the direct
/// container-equals-reg case, or a fully-vetted sub-register context.
enum SubRegOutcome {
    Direct { container_reg: rsleigh::Vn },
    SubReg(SubRegContext),
}

/// Sub-register entry context produced by the shared prelude check, consumed
/// by the read / write specialisations.
struct SubRegContext {
    container_reg: rsleigh::Vn,
    container_ty: ValueType,
    shift_bits: u64,
}

impl<R: rsleigh::MemReader> FunctionLifter<'_, R> {
    /// Builds an address-width integer constant for `off` in `space`.
    ///
    /// The constant's width is the address size of `space` (queried from
    /// Sleigh's `space_info`). `what` names the space in the error when the
    /// lookup fails, preserving each call site's diagnostic.
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

    /// Reads a sequence of varnodes into IR values, preserving order.
    pub(crate) fn read_vns(&mut self, vns: &[rsleigh::Vn]) -> Result<Vec<strider_ir::Value>> {
        vns.iter().map(|vn| self.read_vn(vn)).collect()
    }

    /// Reads the value of input varnode `n` of `insn` (checked index).
    pub(super) fn read_input(
        &mut self,
        insn: &rsleigh::Insn,
        n: usize,
    ) -> Result<strider_ir::Value> {
        let vn = crate::lift::pcode_util::nth_input_or_err(insn, n)?;
        self.read_vn(vn)
    }

    /// Reads any varnode into an IR value.
    ///
    /// Dispatches based on the varnode's address space:
    /// - `CONST` → an integer constant node.
    /// - `UNIQUE` / `REGISTER` → the register-aliasing read path
    ///   ([`Self::read_reg_vn`]): sub-register views are sliced out of their
    ///   largest tracked container (Sleigh occasionally writes a wide unique
    ///   and reads a narrow slice of it — e.g. MIPS MULT writes a 64-bit
    ///   unique then Copy reads a 32-bit slice).
    /// - `RAM` → a `Load` from the RAM address space.
    ///
    /// # Errors
    ///
    /// Returns an error when the varnode lives in an unsupported address
    /// space, has an unsupported size, or the IR builder rejects the
    /// resulting node.
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

    /// Writes an IR value into any writable varnode.
    ///
    /// Dispatches based on the varnode's address space:
    /// - `CONST` → error (constants cannot be written).
    /// - `UNIQUE` / `REGISTER` → the register-aliasing write path
    ///   ([`Self::write_reg_vn`]).
    /// - `RAM` → a `Store` to the RAM address space.
    ///
    /// # Errors
    ///
    /// Returns an error when the varnode lives in an unsupported or
    /// non-writable address space, has an unsupported size, or the IR
    /// builder rejects the resulting node.
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

    // ── register-aliasing read/write (moved from strider-ir) ─────────────────

    /// Finds the largest tracked variable in the same space that fully
    /// contains `reg`.
    ///
    /// For REGISTER space this is the architectural register containment
    /// (e.g. `al` -> `rax` on x86-64).  For UNIQUE space the same
    /// containment logic applies — Sleigh sometimes writes a wider unique
    /// varnode and reads a narrow slice of it (e.g. MIPS MULT writes a
    /// 64-bit unique and the next instruction Copies a 4-byte slice to a
    /// register).  Without this aliasing the narrow read returns an
    /// undefined InitialVar.
    ///
    /// # Errors
    ///
    /// Returns an error if `reg` is not in a fixed-offset space (REGISTER
    /// or UNIQUE).
    fn find_largest_fitting_register(&self, reg: &rsleigh::Vn) -> Result<rsleigh::Vn> {
        let space = reg.addr_space;
        if space != rsleigh::VnSpace::REGISTER && space != rsleigh::VnSpace::UNIQUE {
            bail!("unsupported varnode space {space:?}");
        }
        // The lifter's `container_of` covers every tracked vn + CC register
        // (fast map hit) and falls back to an `all_vns` containment scan for
        // ad-hoc vns.  It returns `reg` unchanged when nothing tracked contains
        // it — for this caller that means `reg` is its own container (a
        // legitimate full-width access).
        Ok(self.container_of(reg))
    }

    /// Computes the bit-shift needed to move `reg`'s bits to/from their
    /// position inside `container_reg`.
    ///
    /// Little-endian: bit position = `8 * (reg.off − container.off)` —
    /// the LSB byte is at offset 0, so shifting right by the byte distance
    /// from the container's start places `reg`'s bits at the bottom.
    ///
    /// Big-endian: the MSB byte is at offset 0, so a sub-register sits
    /// `(container.size − reg.size − (reg.off − container.off))` bytes above
    /// the LSB.  Multiplied by 8 this is the right-shift count.
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

    /// Emits IR nodes to read the value of a register varnode.
    ///
    /// If `reg` is a sub-register (e.g. `al` inside `rax`) the method reads
    /// the container register and inserts a right-shift to extract the
    /// relevant bits.  If `reg` is already the container (or is its own
    /// largest container) the value is returned directly.
    ///
    /// # Errors
    ///
    /// Returns an error when `reg` is not in REGISTER / UNIQUE space, has no
    /// enclosing tracked container, is a sub-slice of a wide (>16-byte)
    /// container, has an unsupported byte size, or an underlying builder
    /// node-construction call fails.
    fn read_reg_vn(&mut self, reg: &rsleigh::Vn) -> Result<Value> {
        let ctx = match self.enter_sub_register(reg, "read_reg_vn")? {
            SubRegOutcome::Direct { container_reg } => {
                // Direct-container read: no aliasing slicing needed.
                return self.builder.read_variable(&container_reg);
            }
            SubRegOutcome::SubReg(ctx) => ctx,
        };
        // Sub-register read: shift the container's bits down to the LSB
        // position, then truncate to the sub-register's width.  Even when
        // the shift is zero (sub at offset 0 of the container), the
        // truncate is required — without it the caller receives the full
        // container width, which breaks downstream type-aware operations
        // (e.g. bitcasting a I64 read to F32 is ill-defined — the widths differ —
        // IntBitsToFloat and the optimizer ends up dropping the chain).
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

    /// Emits IR nodes to write `val` into a register varnode.
    ///
    /// If `reg` is a sub-register the method:
    /// 1. Reads the current container value.
    /// 2. Extends `val` to container width and shifts it into reg's bit slot.
    /// 3. Masks the bits *not* belonging to `reg` from the container.
    /// 4. ORs the two together and writes back to the container.
    ///
    /// All masks are computed in **container coordinates** — the reg's
    /// `vn_mask` (always low-bits domain) is shifted by `shift_bits` to land
    /// at reg's actual position inside the container, and the container's
    /// "preserve" mask is the complement.  Without this positioning, an
    /// upper-half write (e.g. AArch64's "FCVT D0,S0 zeroes upper 64 bits of
    /// V0") inverts the mask and silently zeros the lower half of the
    /// container.
    ///
    /// If `reg` is equal to its own container the write is direct.
    ///
    /// # Errors
    ///
    /// Returns an error when `reg` is not in REGISTER / UNIQUE space, has no
    /// enclosing tracked container, is a sub-slice of a wide (>16-byte)
    /// container, has an unsupported byte size, or an underlying builder
    /// node-construction call fails.
    fn write_reg_vn(&mut self, reg: &rsleigh::Vn, val: Value) -> Result<()> {
        let ctx = match self.enter_sub_register(reg, "write_reg_vn")? {
            SubRegOutcome::Direct { container_reg: _ } => {
                // Direct full-container write.  Register variables hold
                // integer-typed values at the register's natural width.
                // Coerce `val` to that width: a same-width value is stored
                // unchanged, a same-width float is bit-reinterpreted, and a
                // 1-bit `I1` (a comparison / flag result, value 0 or 1) is
                // zero-extended to the register width.  This guarantees no
                // sub-width value — notably `I1` — ever lives in a register
                // SSA slot, so cross-region `Phi`s over a register are
                // type-homogeneous.  The flag→`If` flow stays correct: the
                // cond-branch lifter narrows the register read back to `I1`,
                // and ConstantFold collapses the extend/truncate round-trip.
                let reg_ty: ValueType = reg.int_type()?;
                let coerced = self.builder.convert_to_int_if_needed(val, reg_ty)?;
                return self.builder.write_variable(reg, coerced);
            }
            SubRegOutcome::SubReg(ctx) => ctx,
        };
        // Coerce `val` to the sub-register's integer width through the SAME
        // prelude the direct-container arm uses (`convert_to_int_if_needed`):
        // a 1-bit `I1` flag result is zero-extended to the sub-register width,
        // and a non-integer `val` (e.g. a scalar-FP write into a SIMD slice)
        // surfaces the identical "bitcast required first" error here rather
        // than the divergent "cannot integer-extend non-integer" that
        // `build_masked_insert`'s `extend_if_needed` would raise.  This makes
        // both write paths accept exactly the same operand-type set.  The
        // subsequent `build_masked_insert` zero-extends this reg-width value to
        // the container width before positioning it.
        let reg_ty: ValueType = reg.int_type()?;
        let val = self.builder.convert_to_int_if_needed(val, reg_ty)?;

        // SOUNDNESS: this sub-register write PRESERVES the bits outside
        // `reg`, and that is correct.  Some ISAs zero the upper bits of the
        // containing vector register on a scalar-FP write (AArch64 `fmov s0`
        // zeroes V0[32:]; x86 VEX `vmovss` zeroes the upper YMM/ZMM) while
        // others preserve them (x86 legacy SSE `movss`).  Crucially, Sleigh
        // models this difference itself by emitting the zeroing as *separate
        // explicit pcode ops* alongside the scalar write — e.g.
        //   AArch64 `fmov s0,w0` →  s0 = Copy(w0); reg(V0[4..]) = Copy(#0)…
        //   x86 VEX  `vmovss`    →  XMM0_Da = …;   ZMM0 = IntZext(XMM0)
        // The lifter processes those zeroing ops as ordinary sub-register
        // writes here, so the resulting IR reflects the exact upper-bits
        // semantics with no per-arch policy needed: preserving within the
        // scalar op is right *because* the zeroing arrives as its own op.
        // (Verified by lifting these instructions and inspecting the pcode.)
        let final_container_value = self.build_masked_insert(val, reg, &ctx)?;
        self.write_reg_vn(&ctx.container_reg, final_container_value)?;
        Ok(())
    }

    /// Runs the shared sub-register entry checks for `reg`.
    ///
    /// Returns:
    /// * `SubRegOutcome::Direct { container_reg }` — `reg` is its own largest
    ///   container; caller takes the direct-container path (no shift / mask
    ///   needed).
    /// * `SubRegOutcome::SubReg(SubRegContext { .. })` — `reg` is a strict
    ///   sub-slice of a ≤16-byte container; caller specialises read (shift
    ///   right + truncate) or write (extend + shift left + mask + OR).
    ///
    /// Surfaces typed errors for the wide-container case (>16 bytes) and the
    /// defensive shift-bound check, both of which are correctness-critical.
    /// `op` is the caller's function name, used as a prefix in the shift-bound
    /// error so the failure points at the originating site.
    fn enter_sub_register(&self, reg: &rsleigh::Vn, op: &'static str) -> Result<SubRegOutcome> {
        let container_reg = self.find_largest_fitting_register(reg)?;
        if container_reg == *reg {
            return Ok(SubRegOutcome::Direct { container_reg });
        }
        // Sub-register reads/writes within a wide (>16-byte) container would
        // need a wide mask + shift, which the current u128-mask path cannot
        // represent.  Bail with a clear error rather than silently producing
        // the wrong value.  Direct full-container access (above) and narrow
        // sub-slice within a ≤16-byte container (below) work normally.
        if container_reg.size > 16 {
            return Err(anyhow!(
                "{op}: sub-register aliasing within a wide ({}-byte) container \
                 is not supported (reg {:?}, container {:?})",
                container_reg.size,
                reg,
                container_reg,
            ));
        }
        let container_ty: ValueType = container_reg.int_type()?;
        let shift_bits = self.calculate_reg_shift_from_container(reg, &container_reg);
        // Defensive bound: any shift ≥ container_bits is undefined per the
        // IR's `ShiftRight` / `ShiftLeft` semantics (the lifted shift would
        // silently wrap via `shift % bit_width` on x86 hardware in release
        // builds).  By construction the largest legitimate sub-register
        // offset is `(container.size - 1) * 8`, well below the bit width,
        // and `find_largest_fitting_register` upstream enforces containment.
        // A malformed Sleigh spec that ever emits an out-of-container
        // sub-register surfaces as a clean lift failure rather than silently
        // corrupting the IR.
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

    /// Positions `val` into the container value at the given bit slot, merges
    /// with the preserved container bits, and returns the combined value.
    ///
    /// Steps:
    ///  1. Zero-extend `val` to `ty`, then shift left by `shift_bits` to place
    ///     it at the correct bit position inside the container.
    ///  2. AND the positioned value with `reg_mask` to isolate its bits.
    ///  3. AND the container's current value with `container_mask` to clear the slot.
    ///  4. OR the two halves together.
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

        // Extend `val` to container width, then shift into position.
        let val_extended = self.builder.extend_if_needed(val, ty, ExtendOp::ZeroExtend)?;
        let shifted_value =
            self.builder
                .build_shift_by_const(val_extended, shift_bits, IntBinaryOp::ShiftLeft, ty)?;

        // Isolate the positioned value's bits (`shifted_value & reg_mask`) and
        // clear that slot in the container (`container_val & container_mask`); both
        // ANDs fold the const + binary-op via `build_const_binop` (And is
        // commutative, so the operand order matches the former explicit form).
        let reg_val = self
            .builder
            .build_const_binop(reg_mask, shifted_value, IntBinaryOp::And, ty)?;
        let preserved = self
            .builder
            .build_const_binop(container_mask, container_val, IntBinaryOp::And, ty)?;

        self.builder
            .build_int_binary_operation(preserved, reg_val, IntBinaryOp::Or, ty)
    }
}

// ── Self-contained unit tests for the bit-shift formulas ──────────────────────
//
// These test pure arithmetic (no IR builder needed).  They live here now
// since the formulas they cover live here.

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

    /// Shift placement for little-endian: byte offset within container × 8.
    /// 4-byte container at off=0 with sub-registers at every (off, size) the
    /// formula must support.
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

    /// Shift placement for big-endian: most-significant byte at offset 0,
    /// least-significant at offset (container.size − sub.size).
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

    /// 8-byte container exercises the wider arithmetic path.
    #[test]
    fn be_shift_for_subregs_in_8byte_container() {
        let container = reg(0, 8);
        assert_eq!(compute_shift_be(&reg(0, 4), &container), 32);
        assert_eq!(compute_shift_be(&reg(4, 4), &container), 0);
        assert_eq!(compute_shift_be(&reg(0, 1), &container), 56);
        assert_eq!(compute_shift_be(&reg(7, 1), &container), 0);
    }

    // Free helpers mirroring calculate_reg_shift_from_container's two arms,
    // unit-testable without spinning up a full lifter.
    fn compute_shift_le(reg: &Vn, container: &Vn) -> u64 {
        8 * (reg.addr_off - container.addr_off)
    }
    fn compute_shift_be(reg: &Vn, container: &Vn) -> u64 {
        8 * (container.size as u64 - reg.size as u64 - (reg.addr_off - container.addr_off))
    }
}

// ── Positioned reg-mask: AArch64 SIMD upper-half write regression tests ──────
//
// Sub-register writes need a mask that picks out **reg's position inside the
// container**, not reg's bits in low-bytes domain.  A pre-fix version used
// `vn_mask(reg)` directly — which is always in low-bytes domain regardless of
// where reg sits inside the container.  For shift==0 (low sub-register) it
// accidentally worked; for shift>0 (e.g. upper 8 bytes of a 16-byte SIMD
// register, written when AArch64 FCVT D0,S0 zeroes the upper half of V0) it
// produced an inverted container_mask that silently zeroed the lower half —
// orphaning the FCVT chain through ConstantFold's
// `(a&C1 | b&C2) & C3 → (a&(C1&C3)) | (b&(C2&C3))` and `x & 0 → 0` rules.

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

    /// Positioned mask = `vn_mask(reg) << shift_bits` must select exactly the
    /// bits reg occupies inside its container.
    #[test]
    fn positioned_mask_isolates_reg_bits_inside_16byte_container() {
        let q0 = reg_at(0, 16);
        let s0 = reg_at(0, 4); // lower 4 bytes
        let d0 = reg_at(0, 8); // lower 8 bytes
        let v0_upper8 = reg_at(8, 8); // upper 8 bytes (the AArch64 SIMD upper-half hot spot)

        let q0_mask = vn_mask(&q0).unwrap();
        assert_eq!(q0_mask, u128::MAX, "container mask should be all-ones");

        let s0_pos = vn_mask(&s0).unwrap(); // shift = 0 → no shift needed
        assert_eq!(s0_pos, 0xFFFF_FFFF, "s0 occupies bits 0..32");

        let d0_pos = vn_mask(&d0).unwrap();
        assert_eq!(d0_pos, 0xFFFF_FFFF_FFFF_FFFF, "d0 occupies bits 0..64");

        let upper8_pos = vn_mask(&v0_upper8).unwrap() << 64;
        assert_eq!(
            upper8_pos, 0xFFFF_FFFF_FFFF_FFFF_0000_0000_0000_0000,
            "upper 8-byte sub at offset 8 occupies bits 64..128"
        );

        // The two halves are disjoint and union to the full container mask.
        assert_eq!(d0_pos & upper8_pos, 0, "d0 and upper-half are disjoint");
        assert_eq!(d0_pos | upper8_pos, q0_mask, "d0 ∪ upper-half = full q0");
    }

    /// `container_mask = vn_mask(container) & !positioned_reg_mask` must
    /// preserve exactly the bits that DON'T belong to reg.
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

    /// `container_mask` for the lower-half write path (shift=0).
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

    /// Scalar-FP upper-bits zeroing is sound because Sleigh emits the
    /// zeroing as separate explicit pcode ops (e.g. `fmov s0,w0` →
    /// `s0 = Copy(w0)` then `reg(V0[4..]) = Copy(#0)`).  The lifter
    /// processes each as an ordinary sub-register masked-insert.  This pins
    /// the invariant that makes that approach exact: the scalar write's
    /// positioned mask and the upper-bytes zero-write's positioned mask are
    /// disjoint and together tile the whole container — so after both writes
    /// the container is fully determined (low = value, upper = 0), with no
    /// preserved-stale bits and no gaps.
    #[test]
    fn aarch64_scalar_fp_write_then_upper_zero_tiles_the_container() {
        // `fmov s0,w0` lifts (per Sleigh) to: s0 = Copy(w0) then aligned
        // zero-writes covering the rest of the container — observed as a
        // 4-byte write at offset 4 and an 8-byte write at offset 8.
        let v0 = reg_at(0, 16);
        let s0 = reg_at(0, 4);
        let zero_mid = reg_at(4, 4); // bytes 4..8
        let zero_hi = reg_at(8, 8); // bytes 8..16

        let s0_pos = vn_mask(&s0).unwrap(); // shift 0
        let mid_pos = vn_mask(&zero_mid).unwrap() << 32;
        let hi_pos = vn_mask(&zero_hi).unwrap() << 64;

        // Pairwise disjoint: the scalar write and each zero-fill touch no
        // common bit (so no write clobbers another).
        assert_eq!(s0_pos & mid_pos, 0);
        assert_eq!(s0_pos & hi_pos, 0);
        assert_eq!(mid_pos & hi_pos, 0);
        // Covering: together they are exactly the full V0 container, so after
        // the scalar write + Sleigh's zero-fills the container is fully
        // determined (low 4 = value, upper 12 = 0) with no stale bits.
        assert_eq!(
            s0_pos | mid_pos | hi_pos,
            vn_mask(&v0).unwrap(),
            "s0 ∪ zero-fills must tile the full V0 container"
        );
    }

    /// 4-byte container with byte-sized sub-registers at each offset —
    /// exercises every shift count the LE formula produces.
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

    /// Masks must exactly cover each supported byte width with no extra bits.
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

    /// 10-byte mask is exactly the low 80 bits.
    #[test]
    fn vn_mask_for_10_bytes_is_low_80_bits() -> Result<()> {
        let mask = vn_mask(&reg(10))?;
        assert_eq!(mask & ((1u128 << 80) - 1), (1u128 << 80) - 1);
        assert_eq!(mask >> 80, 0);
        assert_eq!(mask, 0x_0000_FFFF_FFFF_FFFF_FFFF_FFFFu128);
        Ok(())
    }

    /// Wider masks are supersets of narrower masks.
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

    /// Every unsupported size produces an "unsupported register size" error.
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

    // A 256-/512-bit mask has no `u128` representation, so `vn_mask` fails
    // closed for ymm/zmm rather than handing back a silently-truncated
    // `u128::MAX`.  These widths are still valid *containers*; a full-width
    // access takes the direct container path (which never consults the mask)
    // and a sub-register slice of a >16-byte container is rejected in
    // `enter_sub_register` before any mask is computed.
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
