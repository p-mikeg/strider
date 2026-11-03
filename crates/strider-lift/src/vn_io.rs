//! The sole owner of register aliasing: overlapping sub-registers (x86-64
//! `al`/`ah`/`ax`/`eax`/`rax`, x87 ST slices, aarch64 s/d/q SIMD views) are
//! always read and written through the largest containing tracked register,
//! with shift/mask operations inserted for the slice.

use anyhow::anyhow;
use strider_ir::node::ValueId;
use strider_ir::{ExtendOp, IRBuilderExt, IntBinaryOp, ValueType, VnTypeExt};

use super::FunctionLifter;
use super::pcode_util::Result;

/// Ghidra's `BUILTIN_VOLATILE_READ`, past any sla's own user-op ids.
const READ_VOLATILE_USER_OP_ID: u64 = 0x1000_0001;

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

    /// UNIQUE goes through the same aliasing path as REGISTER: Sleigh
    /// sometimes writes a wide unique and reads a narrow slice of it (MIPS
    /// MULT writes a 64-bit unique, then Copy reads 32 bits of it).
    pub(crate) fn read_vn(&mut self, vn: &rsleigh::Vn) -> Result<ValueId> {
        let space = vn.addr_space;
        match space {
            rsleigh::VnSpace::CONST => self.builder.build_int_const(vn.addr_off, vn.int_type()?),
            rsleigh::VnSpace::REGISTER if self.lifter.is_volatile(vn) => self.read_volatile(vn),
            // The two arms spell `vn_container::is_aliasable_space` out: a
            // function cannot be a pattern, and a guard reads worse here.
            rsleigh::VnSpace::UNIQUE | rsleigh::VnSpace::REGISTER => self.read_reg_vn(vn),
            rsleigh::VnSpace::RAM => {
                let addr = self.build_addr_const(space, vn.addr_off, "RAM space")?;
                self.builder.build_load(addr, space, vn.int_type()?)
            }
            _ => Err(anyhow!("unsupported varnode space {space:?}")),
        }
    }

    /// A read of a register that changes with nothing writing it: a fresh
    /// value each time, from a `CallOther` on the control chain, so no two
    /// reads merge. Ghidra's decompiler lifts it the same way, as its builtin
    /// `read_volatile` user-op. A write stays a plain register write: no read
    /// observes it.
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
        // Stash an ISA-mode commit for `pending_isa_mode`. Full `Vn` equality,
        // not just space+offset: a differently-sized varnode sharing the offset
        // is a different register.
        if self.lifter.tables().isa_mode_switch_vn == Some(*vn)
            && let Some(addr) = self.builder.lift_addr()
        {
            // FIRST write per machine address wins. MIPS writes ISAModeSwitch
            // a second time AFTER its `BranchIndirect`, a delay-slot artefact
            // the branch never reads. ARM's call-shaped `ldr pc,addrmode2`
            // commits the loaded target's low bit, calls, then commits a
            // constant 0, all at one address
            // (`ARMinstructions.sinc:2788`-`:2791`).
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
        // The truncate is required even at shift 0: without it the caller gets
        // the full container width, and width-sensitive downstream ops break
        // (bitcasting an I64 read to F32 is ill-defined, so IntBitsToFloat
        // plus the optimizer drop the chain).
        let reg_ty: ValueType = reg.int_type()?;
        let shifted = self.builder.build_shift_by_const(
            container_val,
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
    pub(crate) fn write_reg_vn(&mut self, reg: &rsleigh::Vn, val: ValueId) -> Result<()> {
        let ctx = self.enter_sub_register(reg, "write_reg_vn")?;
        // Register SSA slots hold integers at the register's natural width, so
        // a 1-bit `I1` flag result is zero-extended up.  A float is rejected
        // outright: every float producer bitcasts before it reaches here.
        // Keeping `I1` out of register slots is what makes cross-region `Phi`s
        // over a register type-homogeneous.  The flag-to-`If` flow survives:
        // `build_branch_condition` tests `!= 0` at the slot's own width.
        let reg_ty: ValueType = reg.int_type()?;
        let val = self.builder.convert_to_int_if_needed(val, reg_ty)?;
        let Some(ctx) = ctx else {
            return self.write_variable(reg, val);
        };

        // This write PRESERVES the bits outside `reg`, and that is correct
        // even on ISAs that zero the rest of the vector register on a
        // scalar-FP write (AArch64 `fmov s0`, x86 VEX `vmovss`) rather than
        // preserving it (legacy SSE `movss`).  Sleigh models the difference
        // itself, emitting the zeroing as separate explicit pcode ops:
        //   AArch64 `fmov s0,w0` ->  s0 = Copy(w0), then four zero-writes
        //                              tiling the rest of the 32-byte `z0`
        //   x86 VEX  `vmovss`    ->  XMM0_Da = ...; ZMM0 = IntZext(XMM0)
        // Those arrive here as ordinary sub-register writes.
        let merged = self.build_masked_insert(val, reg, &ctx)?;
        self.write_variable(&ctx.container_reg, merged)
    }

    /// Shared read/write prelude: the largest tracked varnode containing
    /// `reg` (`al` -> `rax`), or `None` when `reg` is its own container.
    /// `op` prefixes the shift-bound error so it names the originating call
    /// site.
    ///
    /// A container byte width with no `ValueType::int_from_bytes` mapping
    /// fails here at `int_type`, before any shift or mask is built on it.
    fn enter_sub_register(
        &self,
        reg: &rsleigh::Vn,
        op: &'static str,
    ) -> Result<Option<SubRegContext>> {
        debug_assert!(
            vn_container::is_aliasable_space(reg.addr_space),
            "{op}: {reg:?} has no container"
        );
        // `container_of` returns `reg` unchanged when nothing tracked contains
        // it: a full-width access.
        let container_reg = self.container_map.container_of(reg);
        if container_reg == *reg {
            return Ok(None);
        }
        let container_ty: ValueType = container_reg.int_type()?;
        // Both guards are unreachable through `container_of` ->
        // `largest_container_in`, which guarantees containment; together they
        // turn a malformed Sleigh spec into a clean lift failure rather than
        // corrupt IR, in release and debug alike.
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
        // A shift >= container bits saturates to zero under the IR's shift
        // semantics, so the slice would read back as nothing.
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

    /// Positions `val` at its bit slot in the container and merges it with the
    /// preserved container bits.
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

        // `shifted_value` needs no mask of its own: coercing to `reg`'s width
        // zeroed every bit above it, and the shift then landed it in exactly
        // the window `container_mask` clears.
        self.builder
            .build_int_binary_operation(preserved, shifted_value, IntBinaryOp::Or, ty)
    }
}
