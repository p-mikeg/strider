use ir::node::NodeOutputType;

use crate::error::{ErrorKind, Result};

use super::super::IrAnalyzer;

impl<'a, R: rsleigh::MemReader> IrAnalyzer<'a, R> {
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
        let result = self
            .builder
            .build_call_other(user_op_id, &args, output_ty)?;
        if let (Some(out_vn), Some(val)) = (insn.output.as_ref(), result) {
            self.write_vn(out_vn, val)?;
        }
        Ok(())
    }

    pub(super) fn handle_segment_op(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        if insn.inputs.len() < 3 {
            return Err(ErrorKind::TooFewInputs(insn.opcode, 3, insn.inputs.len()).into());
        }
        let id_vn = &insn.inputs[0];
        if id_vn.addr.space != rsleigh::VnSpace::CONST {
            return Err(ErrorKind::ExpectedConstInput(insn.opcode, 0).into());
        }
        let op_id = id_vn.addr.off;
        let segment = self.read_vn(&insn.inputs[1])?;
        let offset = self.read_vn(&insn.inputs[2])?;
        let out_vn = insn
            .output
            .as_ref()
            .ok_or(ErrorKind::MissingOutputVn(insn.opcode))?;
        let out = self.builder.build_segment_op(
            op_id,
            segment,
            offset,
            out_vn.size.try_into()?,
        )?;
        self.write_vn(out_vn, out)
    }

    pub(super) fn handle_cpool_ref(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        let refs: Vec<ir::Value> = insn
            .inputs
            .iter()
            .map(|vn| self.read_vn(vn))
            .collect::<Result<_>>()?;
        let out_vn = insn
            .output
            .as_ref()
            .ok_or(ErrorKind::MissingOutputVn(insn.opcode))?;
        let out = self
            .builder
            .build_cpool_ref(&refs, out_vn.size.try_into()?)?;
        self.write_vn(out_vn, out)
    }

    pub(super) fn handle_new(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        let args: Vec<ir::Value> = insn
            .inputs
            .iter()
            .map(|vn| self.read_vn(vn))
            .collect::<Result<_>>()?;
        let out_vn = insn
            .output
            .as_ref()
            .ok_or(ErrorKind::MissingOutputVn(insn.opcode))?;
        let out = self.builder.build_new(&args, out_vn.size.try_into()?)?;
        self.write_vn(out_vn, out)
    }
}
