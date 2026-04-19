use ir::{ExtendOp, IntBinaryOp};

use crate::error::{ErrorKind, Result};

use super::IrAnalyzer;

impl<'a, R: rsleigh::MemReader> IrAnalyzer<'a, R> {
    /// Finds the largest architectural register that fully contains `reg`.
    ///
    /// For example, given `al` (offset 0, size 1) this returns `rax`
    /// (offset 0, size 8) because x86 reads/writes to `al` always go through
    /// `rax`.  The returned register is the widest one in the variable set
    /// that completely covers `reg`'s byte range.
    ///
    /// Returns `None` only if no variable in the builder covers the range,
    /// which should never happen because a register must cover itself.
    pub(super) fn find_largest_fitting_register(
        &self,
        reg: &rsleigh::Vn,
    ) -> Result<Option<rsleigh::Vn>> {
        if reg.addr.space != rsleigh::VnSpace::REGISTER {
            return Err(ErrorKind::UnsupportedVnSpace(reg.addr.space).into());
        }
        let reg_start = reg.addr.off;
        let reg_end = reg_start + reg.size as u64;
        let mut largest_reg_container: Option<rsleigh::Vn> = None;
        for sleigh_reg in self.builder.variables() {
            if !matches!(sleigh_reg.addr.space, rsleigh::VnSpace::REGISTER) {
                continue;
            }
            let sleigh_reg_start = sleigh_reg.addr.off;
            let sleigh_reg_end = sleigh_reg_start + sleigh_reg.size as u64;

            if sleigh_reg_start > reg_start {
                continue;
            }
            if sleigh_reg_end < reg_end {
                continue;
            }
            // We know now that the reg is contained by sleigh reg
            if let Some(reg_container) = largest_reg_container {
                // If the current container is larger - choose it
                if reg_container.size < sleigh_reg.size {
                    largest_reg_container = Some(*sleigh_reg);
                }
            } else {
                largest_reg_container = Some(*sleigh_reg);
            }
        }
        Ok(largest_reg_container)
    }

    /// Computes the bit-shift needed to move `reg`'s bits to/from their
    /// position inside `container_reg`.
    ///
    /// For little-endian architectures the shift is simply
    /// `8 * (reg.off − container.off)`.  For big-endian the shift accounts
    /// for the container's total size so that the most-significant byte comes
    /// first.
    pub(super) fn calculate_reg_shift_from_container(
        &self,
        reg: &rsleigh::Vn,
        container_reg: &rsleigh::Vn,
    ) -> u64 {
        match self.analyzer.arch.endianess {
            crate::arch::Endianess::Little => 8 * (reg.addr.off - container_reg.addr.off),
            crate::arch::Endianess::Big => {
                8 * (container_reg.size as u64 - (reg.addr.off - container_reg.addr.off))
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
        let container_reg = self
            .find_largest_fitting_register(reg)?
            .ok_or(ErrorKind::NoRegisterContainer(*reg))?;
        let curr_reg_val = self.builder.read_variable(&container_reg)?;
        let mut read_reg_val = curr_reg_val;
        if container_reg != *reg {
            // We need to shift the value if it is in the middle of a register
            let shift_value = self.calculate_reg_shift_from_container(reg, &container_reg);
            if shift_value != 0 {
                let shift_const = self
                    .builder
                    .build_int_const(shift_value, container_reg.size.try_into()?);
                read_reg_val = self.builder.build_int_binary_operation(
                    curr_reg_val,
                    shift_const,
                    IntBinaryOp::ShiftRight,
                    reg.size.try_into()?,
                )?;
            }
        }

        Ok(read_reg_val)
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
        let container_reg = self
            .find_largest_fitting_register(reg)?
            .ok_or(ErrorKind::NoRegisterContainer(*reg))?;
        if container_reg == *reg {
            return Ok(self.builder.write_variable(reg, val)?);
        }
        let container_reg_val = self.builder.read_variable(&container_reg)?;

        // The register is in the part of a bigger container
        let shift_bits = self.calculate_reg_shift_from_container(reg, &container_reg);

        // Calculate the shifted value that should be in the reg inside the container
        let shifted_value = if shift_bits == 0 {
            self.builder.extend_if_needed(
                val,
                container_reg.size.try_into()?,
                ExtendOp::ZeroExtend,
            )?
        } else {
            let shift_const = self
                .builder
                .build_int_const(shift_bits, container_reg.size.try_into()?);
            self.builder.build_int_binary_operation(
                val,
                shift_const,
                IntBinaryOp::ShiftLeft,
                reg.size.try_into()?,
            )?
        };

        // Calculate the masked value of the reg in the container
        let reg_mask = crate::utils::vn_mask(reg)?;
        let reg_mask_val = self
            .builder
            .build_int_const(reg_mask, container_reg.size.try_into()?);
        let reg_val = self.builder.build_int_binary_operation(
            reg_mask_val,
            shifted_value,
            IntBinaryOp::And,
            container_reg.size.try_into()?,
        )?;

        // Calculate the rest of the container
        let container_mask = crate::utils::vn_mask(&container_reg)? & (!reg_mask);
        let container_mask_val = self
            .builder
            .build_int_const(container_mask, container_reg.size.try_into()?);
        let container_val = self.builder.build_int_binary_operation(
            container_mask_val,
            container_reg_val,
            IntBinaryOp::And,
            container_reg.size.try_into()?,
        )?;

        // Merge the containers
        let final_container_value = self.builder.build_int_binary_operation(
            container_val,
            reg_val,
            IntBinaryOp::Or,
            container_reg.size.try_into()?,
        )?;
        self.write_reg_vn(&container_reg, final_container_value)?;
        Ok(())
    }
}
