use anyhow::Result;
use strider_ir::{IRBuilderExt, VnTypeExt};

use crate::FunctionLifter;
use crate::pcode_consts::opaque_clobber_set;
use crate::pcode_util::require_output_vn;

impl<R: rsleigh::MemReader> FunctionLifter<'_, R> {
    /// A constant-space LOAD yields its address (GHIDRA's `IPTR_CONSTANT`), as
    /// ppc64 `rldic` / `rldimi` / `rldcr` masks use.
    pub(super) fn handle_load(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        let space = self.lifter.decode_space_id(insn)?;
        if space == rsleigh::VnSpace::REGISTER {
            if let Some(source) = self.lifter.register_slot_of(insn, &self.pcode_consts) {
                let out_vn = require_output_vn(insn)?;
                let value = self.read_vn(&source)?;
                return self.write_vn(out_vn, value);
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
        let space = self.lifter.decode_space_id(insn)?;
        if space == rsleigh::VnSpace::REGISTER {
            if let Some(target) = self.lifter.register_slot_of(insn, &self.pcode_consts) {
                let data = self.read_input(insn, 2)?;
                return self.write_vn(&target, data);
            }
            return self.opaque_register_store(insn);
        }
        let addr = self.read_input(insn, 1)?;
        let data = self.read_input(insn, 2)?;
        self.builder.build_store(addr, data, space)
    }

    /// A register-space LOAD whose address names no register (ppc `mfsrin`):
    /// a memory load after mirroring the registers into the space.
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

    /// A register-space STORE whose address names no register: mirror the
    /// registers into the space, store, and re-read every tracked register
    /// from it.
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
            // At the space's width: at `ty` distinct offsets could mask to one
            // slot (ppc `cr0` 0x900 and `xer_so` 0x400).
            let slot =
                self.build_addr_const(rsleigh::VnSpace::REGISTER, vn.addr_off, "REGISTER space")?;
            let value = self
                .builder
                .build_load(slot, rsleigh::VnSpace::REGISTER, ty)?;
            self.write_vn(&vn, value)?;
        }
        Ok(())
    }

    /// Stores every tracked register into its REGISTER-space slot, which
    /// named register writes (SSA variables) leave stale.
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

    pub(super) fn handle_cpool_ref(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        let refs = self.read_vns(&insn.inputs)?;
        let out_vn = require_output_vn(insn)?;
        let result = self.builder.build_cpool_ref(&refs, out_vn.int_type()?)?;
        self.write_vn(out_vn, result)
    }

    pub(super) fn handle_new(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        let args = self.read_vns(&insn.inputs)?;
        let out_vn = require_output_vn(insn)?;
        let result = self.builder.build_new(&args, out_vn.int_type()?)?;
        self.write_vn(out_vn, result)
    }
}
