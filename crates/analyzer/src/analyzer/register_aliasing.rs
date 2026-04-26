use ir::{ExtendOp, IntBinaryOp};

use crate::error::{ErrorKind, Result};

use super::IrAnalyzer;

impl<'a, R: rsleigh::MemReader> IrAnalyzer<'a, R> {
    /// Finds the largest variable in the same space that fully contains `reg`.
    ///
    /// For REGISTER space this is the architectural register containment
    /// (e.g. `al` -> `rax` on x86-64).  For UNIQUE space the same containment
    /// logic applies — Sleigh sometimes writes a wider unique varnode and
    /// reads a narrow slice of it (e.g. MIPS MULT writes a 64-bit unique
    /// and the next instruction Copies a 4-byte slice to a register).
    /// Without this aliasing the narrow read returns an undefined InitialVar.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorKind::UnsupportedVnSpace`] if `reg` is not in a
    /// fixed-offset space (REGISTER or UNIQUE).  Returns
    /// [`ErrorKind::NoRegisterContainer`] if no variable in the builder
    /// covers `reg`'s byte range — this should never happen because every
    /// varnode at least contains itself.
    pub(super) fn find_largest_fitting_register(
        &self,
        reg: &rsleigh::Vn,
    ) -> Result<rsleigh::Vn> {
        let space = reg.addr.space;
        if space != rsleigh::VnSpace::REGISTER && space != rsleigh::VnSpace::UNIQUE {
            return Err(ErrorKind::UnsupportedVnSpace(space).into());
        }
        let reg_start = reg.addr.off;
        let reg_end = reg_start + reg.size as u64;
        let mut best: Option<rsleigh::Vn> = None;
        for sleigh_reg in self.builder.variables() {
            if sleigh_reg.addr.space != space {
                continue;
            }
            let s = sleigh_reg.addr.off;
            let e = s + sleigh_reg.size as u64;
            if s > reg_start || e < reg_end {
                continue;
            }
            // Contained.  Take it if it's strictly wider than the current best.
            if best.is_none_or(|b| b.size < sleigh_reg.size) {
                best = Some(*sleigh_reg);
            }
        }
        best.ok_or_else(|| ErrorKind::NoRegisterContainer(*reg).into())
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
    pub(super) fn calculate_reg_shift_from_container(
        &self,
        reg: &rsleigh::Vn,
        container_reg: &rsleigh::Vn,
    ) -> u64 {
        match self.analyzer.arch.endianness {
            crate::Endianness::Little => 8 * (reg.addr.off - container_reg.addr.off),
            crate::Endianness::Big => {
                8 * (container_reg.size as u64
                    - reg.size as u64
                    - (reg.addr.off - container_reg.addr.off))
            }
        }
    }

    /// Emits IR nodes to read the value of a register varnode.
    ///
    /// If `reg` is a sub-register (e.g. `al` inside `rax`) the method reads
    /// the container register and inserts a right-shift to extract the
    /// relevant bits.  If `reg` is already the container (or is its own
    /// largest container) the value is returned directly.
    pub(super) fn read_reg_vn(&mut self, reg: &rsleigh::Vn) -> Result<ir::Value> {
        let container_reg = self.find_largest_fitting_register(reg)?;
        let curr_reg_val = self.builder.read_variable(&container_reg)?;
        if container_reg == *reg {
            return Ok(curr_reg_val);
        }
        // Sub-register read: shift the container's bits down to the LSB
        // position, then truncate to the sub-register's width.  Even when
        // the shift is zero (sub at offset 0 of the container), the
        // truncate is required — without it the caller receives the full
        // container width, which breaks downstream type-aware operations
        // (e.g. CastToFloat(F32) on a U64 input cannot lower to a clean
        // IntBitsToFloat and the optimizer ends up dropping the chain).
        let reg_ty: ir::ValueType = reg.size.try_into()?;
        let shift_value = self.calculate_reg_shift_from_container(reg, &container_reg);
        let shifted = if shift_value == 0 {
            curr_reg_val
        } else {
            let shift_const = self
                .builder
                .build_int_const(shift_value, container_reg.size.try_into()?);
            self.builder.build_int_binary_operation(
                curr_reg_val,
                shift_const,
                IntBinaryOp::ShiftRight,
                container_reg.size.try_into()?,
            )?
        };
        Ok(self.builder.truncate_if_needed(shifted, reg_ty)?)
    }

    /// Emits IR nodes to write `val` into a register varnode.
    ///
    /// If `reg` is a sub-register the method:
    /// 1. Reads the current container value.
    /// 2. Shifts and masks `val` into the correct bit range.
    /// 3. Masks out the old bits of `reg` inside the container.
    /// 4. ORs the two together and writes back to the container.
    ///
    /// If `reg` is equal to its own container the write is direct.
    pub(super) fn write_reg_vn(&mut self, reg: &rsleigh::Vn, val: ir::Value) -> Result<()> {
        let container_reg = self.find_largest_fitting_register(reg)?;
        if container_reg == *reg {
            return Ok(self.builder.write_variable(reg, val)?);
        }
        let container_ty: ir::ValueType = container_reg.size.try_into()?;
        let container_reg_val = self.builder.read_variable(&container_reg)?;
        let shift_bits = self.calculate_reg_shift_from_container(reg, &container_reg);

        // Position `val`'s bits inside the container's bit window.
        let shifted_value = if shift_bits == 0 {
            self.builder
                .extend_if_needed(val, container_ty, ExtendOp::ZeroExtend)?
        } else {
            let shift_const = self.builder.build_int_const(shift_bits, container_ty);
            self.builder.build_int_binary_operation(
                val,
                shift_const,
                IntBinaryOp::ShiftLeft,
                reg.size.try_into()?,  // intentionally reg's size: shifting in reg's domain before merging
            )?
        };

        // Mask `val` to its declared width inside the container.
        let reg_mask = crate::utils::vn_mask(reg)?;
        let reg_mask_val = self.builder.build_int_const(reg_mask, container_ty);
        let reg_val = self.builder.build_int_binary_operation(
            reg_mask_val,
            shifted_value,
            IntBinaryOp::And,
            container_ty,
        )?;

        // Mask out the old bits of `reg` from the container.
        let container_mask = crate::utils::vn_mask(&container_reg)? & !reg_mask;
        let container_mask_val = self.builder.build_int_const(container_mask, container_ty);
        let container_val = self.builder.build_int_binary_operation(
            container_mask_val,
            container_reg_val,
            IntBinaryOp::And,
            container_ty,
        )?;

        // Merge.
        let final_container_value = self.builder.build_int_binary_operation(
            container_val,
            reg_val,
            IntBinaryOp::Or,
            container_ty,
        )?;
        self.write_reg_vn(&container_reg, final_container_value)?;
        Ok(())
    }
}

#[cfg(test)]
mod shift_formula_tests {
    use rsleigh::{Vn, VnAddr, VnSpace};

    fn reg(off: u64, size: u32) -> Vn {
        Vn { addr: VnAddr { off, space: VnSpace::REGISTER }, size }
    }

    /// Shift placement for little-endian: byte offset within container × 8.
    /// 4-byte container at off=0 with sub-registers at every (off, size) the
    /// formula must support.
    #[test]
    fn le_shift_for_subregs_in_4byte_container() {
        let cases = [
            (0, 1, 0),  (1, 1, 8),  (2, 1, 16), (3, 1, 24),
            (0, 2, 0),  (2, 2, 16), (0, 4, 0),
        ];
        for (sub_off, sub_size, expected) in cases {
            let container = reg(0, 4);
            let sub = reg(sub_off, sub_size);
            let shift = compute_shift_le(&sub, &container);
            assert_eq!(shift, expected,
                "LE sub({sub_off},{sub_size}): expected {expected}, got {shift}");
        }
    }

    /// Shift placement for big-endian: most-significant byte at offset 0,
    /// least-significant at offset (container.size − sub.size).
    #[test]
    fn be_shift_for_subregs_in_4byte_container() {
        let cases = [
            (0, 1, 24), (1, 1, 16), (2, 1, 8), (3, 1, 0),
            (0, 2, 16), (2, 2, 0),  (0, 4, 0),
        ];
        for (sub_off, sub_size, expected) in cases {
            let container = reg(0, 4);
            let sub = reg(sub_off, sub_size);
            let shift = compute_shift_be(&sub, &container);
            assert_eq!(shift, expected,
                "BE sub({sub_off},{sub_size}): expected {expected}, got {shift}");
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
    // unit-testable without spinning up a full IrAnalyzer.
    fn compute_shift_le(reg: &Vn, container: &Vn) -> u64 {
        8 * (reg.addr.off - container.addr.off)
    }
    fn compute_shift_be(reg: &Vn, container: &Vn) -> u64 {
        8 * (container.size as u64 - reg.size as u64 - (reg.addr.off - container.addr.off))
    }
}
