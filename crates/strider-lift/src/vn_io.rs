use anyhow::{Result, anyhow};
use strider_ir::node::ValueId;
use strider_ir::{ExtendOp, IRBuilderExt, IntBinaryOp, ValueType, VnTypeExt};

use super::FunctionLifter;

/// Ghidra's `BUILTIN_VOLATILE_READ`, past any sla's own user-op ids.
const READ_VOLATILE_USER_OP_ID: u64 = 0x1000_0001;

struct SubRegContext {
    container_reg: rsleigh::Vn,
    container_ty: ValueType,
    shift_bits: u64,
}

impl<R: rsleigh::MemReader> FunctionLifter<'_, R> {
    /// An address constant at `space`'s address width.
    pub(crate) fn build_addr_const(
        &mut self,
        space: rsleigh::VnSpace,
        off: u64,
        what: &str,
    ) -> Result<ValueId> {
        let space_info = self
            .lifter
            .sleigh()
            .space_info(space)
            .ok_or_else(|| anyhow!("no space info for {what} {space:?}"))?;
        self.builder.build_int_const(
            off,
            strider_ir::ValueType::int_from_bytes(space_info.addr_size())?,
        )
    }

    pub(crate) fn read_vns(&mut self, vns: &[rsleigh::Vn]) -> Result<Vec<ValueId>> {
        vns.iter().map(|vn| self.read_vn(vn)).collect()
    }

    pub(super) fn read_input(&mut self, insn: &rsleigh::Insn, n: usize) -> Result<ValueId> {
        let vn = crate::pcode_util::nth_input_or_err(insn, n)?;
        self.read_vn(vn)
    }

    /// UNIQUE aliases like REGISTER: MIPS `mult` writes a 64-bit unique and
    /// reads 32 bits of it.
    pub(crate) fn read_vn(&mut self, vn: &rsleigh::Vn) -> Result<ValueId> {
        let space = vn.addr_space;
        match space {
            rsleigh::VnSpace::CONST => self.builder.build_int_const(vn.addr_off, vn.int_type()?),
            rsleigh::VnSpace::REGISTER if self.lifter.is_volatile(vn) => self.read_volatile(vn),
            // The `vn_container::is_aliasable_space` spaces.
            rsleigh::VnSpace::UNIQUE | rsleigh::VnSpace::REGISTER => self.read_reg_vn(vn),
            rsleigh::VnSpace::RAM => {
                let addr = self.build_addr_const(space, vn.addr_off, "RAM space")?;
                self.builder.build_load(addr, space, vn.int_type()?)
            }
            _ => Err(anyhow!("unsupported varnode space {space:?}")),
        }
    }

    /// A volatile register read: a fresh `read_volatile` `CallOther` each time
    /// (as in Ghidra's decompiler), so no two reads merge. Writes stay plain.
    fn read_volatile(&mut self, vn: &rsleigh::Vn) -> Result<ValueId> {
        let (node, outputs) =
            self.builder
                .build_call_other(READ_VOLATILE_USER_OP_ID, &[], &[*vn], false, false)?;
        if self.builder.function().call_other_name(node).is_none() {
            self.builder
                .function_mut()
                .set_call_other_name(READ_VOLATILE_USER_OP_ID, "read_volatile");
        }
        Ok(outputs[0])
    }

    pub(crate) fn write_vn(&mut self, vn: &rsleigh::Vn, val: ValueId) -> Result<()> {
        // Stash an ISA-mode commit (full `Vn` equality: size matters).
        if self.lifter.tables().isa_mode_switch_vn == Some(*vn)
            && let Some(addr) = self.builder.lift_addr()
        {
            // First write per machine address wins: MIPS rewrites it after
            // the branch (delay slot), and ARM `ldr pc,addrmode2` commits a
            // constant 0 after the call.
            if !matches!(self.pending_isa_mode, Some((_, prev)) if prev == addr) {
                self.pending_isa_mode = Some((val, addr));
            }
        }
        let space = vn.addr_space;
        match space {
            rsleigh::VnSpace::CONST => Err(anyhow!("write to a CONST varnode {vn:?}")),
            rsleigh::VnSpace::UNIQUE | rsleigh::VnSpace::REGISTER => self.write_reg_vn(vn, val),
            rsleigh::VnSpace::RAM => {
                let addr = self.build_addr_const(space, vn.addr_off, "RAM space")?;
                self.builder.build_store(addr, val, space)
            }
            _ => Err(anyhow!("unsupported varnode space {space:?}")),
        }
    }

    /// Reads `reg` by shifting its slice out of the container and truncating.
    pub(crate) fn read_reg_vn(&mut self, reg: &rsleigh::Vn) -> Result<ValueId> {
        let Some(ctx) = self.enter_sub_register(reg, "read_reg_vn")? else {
            return self.builder.read_variable(reg);
        };
        let curr_reg_val = self.builder.read_variable(&ctx.container_reg)?;
        self.slice_out(curr_reg_val, reg, &ctx)
    }

    /// `reg`'s incoming value: its container's `InitialVar`, sliced. Unlike
    /// [`Self::read_reg_vn`] it never reads the current region, whose value at
    /// a loop-header entry is the phi.
    pub(crate) fn read_initial_reg_vn(&mut self, reg: &rsleigh::Vn) -> Result<Option<ValueId>> {
        let ctx = self.enter_sub_register(reg, "read_initial_reg_vn")?;
        let container_reg = ctx.as_ref().map_or(*reg, |ctx| ctx.container_reg);
        let Some(initial) = self.builder.function().initial_var_value(&container_reg) else {
            return Ok(None);
        };
        match ctx {
            None => Ok(Some(initial)),
            Some(ctx) => self.slice_out(initial, reg, &ctx).map(Some),
        }
    }

    fn slice_out(
        &mut self,
        container_val: ValueId,
        reg: &rsleigh::Vn,
        ctx: &SubRegContext,
    ) -> Result<ValueId> {
        // Truncate even at shift 0: the caller needs `reg`'s width.
        let reg_ty: ValueType = reg.int_type()?;
        let shifted = self.builder.build_shift_by_const(
            container_val,
            ctx.shift_bits,
            IntBinaryOp::ShiftRight,
            ctx.container_ty,
        )?;
        self.builder.truncate_if_needed(shifted, reg_ty)
    }

    /// Writes `val` into `reg`'s slot of its container, preserving the other
    /// bits. Sleigh emits any zeroing of the rest (AArch64 `fmov s0`, VEX
    /// `vmovss`) as separate writes.
    pub(crate) fn write_reg_vn(&mut self, reg: &rsleigh::Vn, val: ValueId) -> Result<()> {
        let ctx = self.enter_sub_register(reg, "write_reg_vn")?;
        // Slots hold integers at the register's width (an `I1` is
        // zero-extended), so a register's `Phi`s are type-homogeneous.
        let reg_ty: ValueType = reg.int_type()?;
        let val = self.builder.convert_to_int_if_needed(val, reg_ty)?;
        let Some(ctx) = ctx else {
            return self.write_variable(reg, val);
        };

        let merged = self.build_masked_insert(val, reg, &ctx)?;
        self.write_variable(&ctx.container_reg, merged)
    }

    /// `reg`'s largest tracked container (`al` -> `rax`), or `None` when `reg`
    /// is its own. `op` names the caller in errors.
    fn enter_sub_register(
        &self,
        reg: &rsleigh::Vn,
        op: &'static str,
    ) -> Result<Option<SubRegContext>> {
        debug_assert!(
            vn_container::is_aliasable_space(reg.addr_space),
            "{op}: {reg:?} has no container"
        );
        let container_reg = self.container_map.container_of(reg);
        if container_reg == *reg {
            return Ok(None);
        }
        let container_ty: ValueType = container_reg.int_type()?;
        // Both guards are unreachable for a well-formed spec.
        let Some(shift_bits) = self
            .lifter
            .arch()
            .register_endianness()
            .slice_shift_bits(reg, &container_reg)
        else {
            return Err(anyhow!(
                "{op}: sub-register {reg:?} is not contained in {container_reg:?}"
            ));
        };
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
        Ok(Some(SubRegContext {
            container_reg,
            container_ty,
            shift_bits,
        }))
    }

    /// `(container & !slot) | (zext(val) << shift)`.
    fn build_masked_insert(
        &mut self,
        val: ValueId,
        reg: &rsleigh::Vn,
        ctx: &SubRegContext,
    ) -> Result<ValueId> {
        let ty = ctx.container_ty;
        let shift_bits = ctx.shift_bits;
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

        let container_mask =
            self.builder
                .build_window_preserve_mask(u64::from(reg.size) * 8, shift_bits, ty)?;
        let preserved = self.builder.build_int_binary_operation(
            container_val,
            container_mask,
            IntBinaryOp::And,
            ty,
        )?;

        // `val` is already at `reg`'s width, so it fits the cleared slot.
        self.builder
            .build_int_binary_operation(preserved, shifted_value, IntBinaryOp::Or, ty)
    }
}
