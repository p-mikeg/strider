//! Miscellaneous opaque value-producing pcode opcodes:
//! `SegmentOp` (segmented-address lookup), `CPoolRef` (JVM constant-pool
//! lookup), and `New` (JVM object allocation).
//!
//! `CallOther` (CPU intrinsics) and `MultiEqual` (decompiler-internal
//! phi) stay in strider — `CallOther` because it touches the memory
//! chain and resolves user-op names against the sleigh context that
//! strider owns; `MultiEqual` because we currently raise it as an error
//! and that's a strider-level concern.

use anyhow::bail;
use strider_ir::IRBuilderExt;

use crate::lift::pcode_util::Result;
use crate::lift::FunctionLifter;

impl<'a, R: rsleigh::MemReader> FunctionLifter<'a, R> {
    /// SegmentOp: segmented-address lookup.
    /// inputs[0] = CONST op id, inputs[1] = segment, inputs[2] = offset.
    pub(super) fn handle_segment_op(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        if insn.inputs.len() < 3 {
            bail!("opcode {:?} has too few inputs: expected at least 3, got {}", insn.opcode, insn.inputs.len());
        }
        let id_vn = crate::lift::pcode_util::nth_input_or_err(insn, 0)?;
        crate::lift::pcode_util::ensure_const_space(id_vn, insn.opcode, "input 0")?;
        let op_id = id_vn.addr_off;
        let segment = self.read_vn(crate::lift::pcode_util::nth_input_or_err(insn, 1)?)?;
        let offset = self.read_vn(crate::lift::pcode_util::nth_input_or_err(insn, 2)?)?;
        let out_vn = crate::lift::pcode_util::require_output_vn(insn)?;
        let result = self.builder.build_segment_op(
            op_id,
            segment,
            offset,
            strider_ir::ValueType::int_for_byte_size(out_vn.size)?,
        )?;
        self.write_vn(out_vn, result)
    }

    /// CPoolRef: JVM constant-pool lookup.  Opaque, variadic refs.
    pub(super) fn handle_cpool_ref(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        let refs: Vec<strider_ir::Value> = insn
            .inputs
            .iter()
            .map(|vn| self.read_vn(vn))
            .collect::<Result<_>>()?;
        let out_vn = crate::lift::pcode_util::require_output_vn(insn)?;
        let result = self
            .builder
            .build_cpool_ref(&refs, strider_ir::ValueType::int_for_byte_size(out_vn.size)?)?;
        self.write_vn(out_vn, result)
    }

    /// New: JVM object allocation.  Opaque.
    pub(super) fn handle_new(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        let args: Vec<strider_ir::Value> = insn
            .inputs
            .iter()
            .map(|vn| self.read_vn(vn))
            .collect::<Result<_>>()?;
        let out_vn = crate::lift::pcode_util::require_output_vn(insn)?;
        let result = self.builder.build_new(&args, strider_ir::ValueType::int_for_byte_size(out_vn.size)?)?;
        self.write_vn(out_vn, result)
    }
}
