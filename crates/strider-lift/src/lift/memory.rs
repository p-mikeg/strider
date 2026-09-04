use strider_ir::{IRBuilderExt, VnTypeExt};

use crate::lift::FunctionLifter;
use crate::lift::pcode_consts::{register_load_source, register_store_target};
use crate::lift::pcode_util::{Result, require_output_vn};

impl<R: rsleigh::MemReader> FunctionLifter<'_, R> {
    /// A LOAD from the constant space has its address AS its value: GHIDRA's
    /// `MemoryState::getValue` returns the offset unread for `IPTR_CONSTANT`.
    /// PowerPC exports its `rlwimi` / `rldimi` rotate masks that way.
    pub(super) fn handle_load(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        let space = crate::lift::pcode_util::decode_space_id(insn)?;
        if space == rsleigh::VnSpace::REGISTER {
            let source = register_load_source(insn, &self.pcode_consts)
                .ok_or_else(|| register_space_error("LOAD"))?;
            let out_vn = *require_output_vn(insn)?;
            let value = self.read_vn(&source)?;
            return self.write_vn(&out_vn, value);
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
            let target = register_store_target(insn, &self.pcode_consts)
                .ok_or_else(|| register_space_error("STORE"))?;
            let data = self.read_input(insn, 2)?;
            return self.write_vn(&target, data);
        }
        let addr = self.read_input(insn, 1)?;
        let data = self.read_input(insn, 2)?;
        self.builder.build_store(addr, data, space)
    }
}

/// A register access whose address does not fold cannot name the register it
/// touches, and treating it as memory leaves that register never read or
/// written with `is_complete()` still true. Failing the function is the honest
/// answer; every sla that uses this idiom builds the address from instruction
/// fields, so it folds in practice.
fn register_space_error(op: &str) -> anyhow::Error {
    anyhow::anyhow!(
        "{op} addresses the register space at an offset that does not fold to a \
         constant, so the register it names is unknown"
    )
}
