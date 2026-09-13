use strider_ir::{IRBuilderExt, VnTypeExt};

use crate::lift::FunctionLifter;
use crate::lift::pcode_consts::{opaque_clobber_set, register_load_source, register_store_target};
use crate::lift::pcode_util::{Result, require_output_vn};

impl<R: rsleigh::MemReader> FunctionLifter<'_, R> {
    /// A LOAD from the constant space has its address AS its value: GHIDRA's
    /// `MemoryState::getValue` returns the offset unread for `IPTR_CONSTANT`.
    /// PowerPC exports its `rlwimi` / `rldimi` rotate masks that way.
    pub(super) fn handle_load(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        let space = crate::lift::pcode_util::decode_space_id(insn)?;
        if space == rsleigh::VnSpace::REGISTER {
            let declared = self.lifter.declared_reg_vns();
            if let Some(source) = register_load_source(insn, &self.pcode_consts, declared) {
                let out_vn = *require_output_vn(insn)?;
                let value = self.read_vn(&source)?;
                return self.write_vn(&out_vn, value);
            }
            return self.opaque_register_load(insn);
        }
        let addr = self.read_input(insn, 1)?;
        let out_vn = require_output_vn(insn)?;
        let out_ty = out_vn.int_type()?;
        let result = if space == rsleigh::VnSpace::CONST {
            self.builder.convert_to_int_if_needed(addr, out_ty)?
        } else {
            self.builder.build_load(addr, space, out_ty)?
        };
        self.write_vn(out_vn, result)
    }

    pub(super) fn handle_store(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        let space = crate::lift::pcode_util::decode_space_id(insn)?;
        if space == rsleigh::VnSpace::REGISTER {
            let declared = self.lifter.declared_reg_vns();
            if let Some(target) = register_store_target(insn, &self.pcode_consts, declared) {
                let data = self.read_input(insn, 2)?;
                return self.write_vn(&target, data);
            }
            return self.opaque_register_store(insn);
        }
        let addr = self.read_input(insn, 1)?;
        let data = self.read_input(insn, 2)?;
        self.builder.build_store(addr, data, space)
    }

    /// A register-space LOAD whose address names no register (ppc `mfsrin`).
    ///
    /// Reads the REGISTER space as memory after mirroring the registers into
    /// it, so the load answers a register's current value when the address
    /// names one, and two loads either side of a named register write do not
    /// forward to one value.
    fn opaque_register_load(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        let addr = self.read_input(insn, 1)?;
        let out_vn = require_output_vn(insn)?;
        let out_ty = out_vn.int_type()?;
        self.mirror_registers_into_space()?;
        let value = self
            .builder
            .build_load(addr, rsleigh::VnSpace::REGISTER, out_ty)?;
        self.write_vn(out_vn, value)
    }

    /// A register-space STORE whose address names no register.
    ///
    /// The registers are mirrored into the space, the STORE lands there, and
    /// every tracked register is re-read out of it. A register the write
    /// misses reads back its current value; the re-read depends on the store,
    /// so the old value forwards only where the optimizer proves the address
    /// misses that slot. A register read repeatedly after the store sees one
    /// value rather than a fresh unknown each time.
    ///
    /// O(tracked registers), and again in `collect_def_sites`, where a def site
    /// per register widens the whole function's iterated dominance frontier.
    /// Narrower needs a bound on the address, which is what this path lacks.
    fn opaque_register_store(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        let addr = self.read_input(insn, 1)?;
        let data = self.read_input(insn, 2)?;
        self.mirror_registers_into_space()?;
        self.builder
            .build_store(addr, data, rsleigh::VnSpace::REGISTER)?;

        let clobbered: Vec<rsleigh::Vn> =
            opaque_clobber_set(self.builder.function().all_vns()).collect();
        for vn in clobbered {
            let ty = vn.int_type()?;
            // Slot width is the SPACE's; at `ty` the offset masks to the
            // register's own width and distinct registers share one slot
            // (ppc `cr0` 0x900 and `xer_so` 0x400 both land on 0).
            let slot =
                self.build_addr_const(rsleigh::VnSpace::REGISTER, vn.addr_off, "REGISTER space")?;
            let value = self
                .builder
                .build_load(slot, rsleigh::VnSpace::REGISTER, ty)?;
            self.write_vn(&vn, value)?;
        }
        Ok(())
    }

    /// Stores every tracked register's current value into its REGISTER-space
    /// slot. A named register write is an SSA-variable write that leaves the
    /// memory chain alone, so the space is otherwise stale at every register
    /// written since the last opaque access.
    fn mirror_registers_into_space(&mut self) -> Result<()> {
        let mirrored: Vec<rsleigh::Vn> =
            opaque_clobber_set(self.builder.function().all_vns()).collect();
        for vn in mirrored {
            let slot =
                self.build_addr_const(rsleigh::VnSpace::REGISTER, vn.addr_off, "REGISTER space")?;
            let value = self.read_vn(&vn)?;
            self.builder
                .build_store(slot, value, rsleigh::VnSpace::REGISTER)?;
        }
        Ok(())
    }
}
