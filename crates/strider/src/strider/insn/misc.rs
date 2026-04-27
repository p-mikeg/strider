use ir::node::NodeOutputType;

use crate::error::{ErrorKind, Result};

use super::super::IrStrider;

impl<'a, R: rsleigh::MemReader> IrStrider<'a, R> {
    pub(super) fn handle_call_other(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        if insn.inputs.is_empty() {
            return Err(ErrorKind::TooFewInputs(insn.opcode, 1, 0).into());
        }
        let id_vn = &insn.inputs[0];
        if id_vn.addr.space != rsleigh::VnSpace::CONST {
            return Err(ErrorKind::ExpectedConstInput(insn.opcode, 0).into());
        }
        let user_op_id = id_vn.addr.off;
        let args: Vec<ir::Value> = insn.inputs[1..]
            .iter()
            .map(|vn| self.read_vn(vn))
            .collect::<Result<_>>()?;
        let output_ty: Option<NodeOutputType> = match insn.output.as_ref() {
            Some(out_vn) => Some(out_vn.size.try_into()?),
            None => None,
        };
        let (node_id, result) = self
            .builder
            .build_call_other(user_op_id, &args, output_ty)?;
        // Resolve the user-op id to its Sleigh-defined name (e.g.
        // `setISAMode`, `LOCK`, `cpuid`) and stash it in the graph's
        // side-table.  Used by `opt::CallOtherElide` to drop CallOthers
        // whose effect is a true no-op in the IR's value/memory model.
        // u32 is sleigh's native id width — anything wider is malformed.
        if let Ok(id_u32) = u32::try_from(user_op_id)
            && let Some(name) = self.cfg.sleigh.user_op_name(id_u32)
        {
            self.builder
                .body_mut()
                .graph
                .set_call_other_name(node_id, name.to_string());
        }
        if let (Some(out_vn), Some(val)) = (insn.output.as_ref(), result) {
            self.write_vn(out_vn, val)?;
        }
        Ok(())
    }
}
