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

use crate::Result;
use crate::ValueLifter;

impl<'a, R: rsleigh::MemReader> ValueLifter<'a, R> {
    /// SegmentOp: segmented-address lookup.
    /// inputs[0] = CONST op id, inputs[1] = segment, inputs[2] = offset.
    pub(super) fn handle_segment_op(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        if insn.inputs.len() < 3 {
            bail!("opcode {:?} has too few inputs: expected at least 3, got {}", insn.opcode, insn.inputs.len());
        }
        let id_vn = &insn.inputs[0];
        if id_vn.addr.space != rsleigh::VnSpace::CONST {
            bail!("opcode {:?} expects a CONST input at position 0", insn.opcode);
        }
        let op_id = id_vn.addr.off;
        let segment = self.read_vn(&insn.inputs[1])?;
        let offset = self.read_vn(&insn.inputs[2])?;
        let out_vn = crate::require_output_vn(insn)?;
        let out = self.builder.build_segment_op(
            op_id,
            segment,
            offset,
            out_vn.size.try_into()?,
        )?;
        self.write_vn(out_vn, out)
    }

    /// CPoolRef: JVM constant-pool lookup.  Opaque, variadic refs.
    pub(super) fn handle_cpool_ref(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        let refs: Vec<ir::Value> = insn
            .inputs
            .iter()
            .map(|vn| self.read_vn(vn))
            .collect::<Result<_>>()?;
        let out_vn = crate::require_output_vn(insn)?;
        let out = self
            .builder
            .build_cpool_ref(&refs, out_vn.size.try_into()?)?;
        self.write_vn(out_vn, out)
    }

    /// New: JVM object allocation.  Opaque.
    pub(super) fn handle_new(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        let args: Vec<ir::Value> = insn
            .inputs
            .iter()
            .map(|vn| self.read_vn(vn))
            .collect::<Result<_>>()?;
        let out_vn = crate::require_output_vn(insn)?;
        let out = self.builder.build_new(&args, out_vn.size.try_into()?)?;
        self.write_vn(out_vn, out)
    }
}
