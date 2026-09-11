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
    /// The value comes out of the REGISTER space as memory, so two accesses at
    /// the SAME address value forward to each other. A named register write is
    /// an SSA-variable write that never advances the memory chain, so the space
    /// is stale at every register written since the last store into it and two
    /// loads either side of one would forward to the same value, claiming the
    /// machine read the same thing twice. Every tracked register is therefore
    /// mirrored into its slot first, which both breaks that forwarding and
    /// makes the load's answer the register's own value when the address does
    /// name it.
    ///
    /// O(tracked registers), the price `opaque_register_store` pays in the
    /// other direction.
    fn opaque_register_load(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        let addr = self.read_input(insn, 1)?;
        let out_vn = require_output_vn(insn)?;
        let out_ty = out_vn.int_type()?;
        let mirrored: Vec<rsleigh::Vn> =
            opaque_clobber_set(self.builder.function().all_vns()).collect();
        for vn in mirrored {
            let slot =
                self.build_addr_const(rsleigh::VnSpace::REGISTER, vn.addr_off, "REGISTER space")?;
            let value = self.read_vn(&vn)?;
            self.builder
                .build_store(slot, value, rsleigh::VnSpace::REGISTER)?;
        }
        let value = self
            .builder
            .build_load(addr, rsleigh::VnSpace::REGISTER, out_ty)?;
        self.write_vn(out_vn, value)
    }

    /// A register-space STORE whose address names no register.
    ///
    /// Two halves, and both are needed. The STORE itself lands in the REGISTER
    /// space so the data stays live and a later opaque load at the same address
    /// forwards from it. Then every tracked register is re-read out of that
    /// space, which is what carries the aliasing: the write went SOMEWHERE in
    /// the register file, so no register may keep the value it held. The
    /// re-read depends on the store, so the optimizer cannot forward the old
    /// value across it, and a register read repeatedly after the store sees one
    /// value rather than a fresh unknown each time.
    ///
    /// O(tracked registers), and again in `collect_def_sites`, where a def site
    /// per register widens the whole function's iterated dominance frontier.
    /// Narrower needs a bound on the address, which is what this path lacks.
    fn opaque_register_store(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        let addr = self.read_input(insn, 1)?;
        let data = self.read_input(insn, 2)?;
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
}
