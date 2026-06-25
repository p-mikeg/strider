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
use strider_ir::{IRBuilderExt, VnTypeExt};

use crate::lift::{
    FunctionLifter,
    pcode_util::{Result, nth_input_or_err, require_output_vn},
};

impl<'a, R: rsleigh::MemReader> FunctionLifter<'a, R> {
    /// SegmentOp: segmented-address lookup.
    /// inputs[0] = CONST op id, inputs[1] = segment, inputs[2] = offset.
    pub(super) fn handle_segment_op(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        if insn.inputs.len() < 3 {
            bail!(
                "opcode {:?} has too few inputs: expected at least 3, got {}",
                insn.opcode,
                insn.inputs.len()
            );
        }
        let id_vn = nth_input_or_err(insn, 0)?;
        crate::lift::pcode_util::ensure_const_space(id_vn, insn.opcode, "input 0")?;
        let op_id = id_vn.addr_off;
        let segment = self.read_input(insn, 1)?;
        let offset = self.read_input(insn, 2)?;
        let out_vn = require_output_vn(insn)?;
        let result = self
            .builder
            .build_segment_op(op_id, segment, offset, out_vn.int_type()?)?;
        self.write_vn(out_vn, result)
    }

    /// CPoolRef: JVM constant-pool lookup.  Opaque, variadic refs.
    pub(super) fn handle_cpool_ref(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        let refs = self.read_vns(&insn.inputs)?;
        let out_vn = require_output_vn(insn)?;
        let result = self.builder.build_cpool_ref(&refs, out_vn.int_type()?)?;
        self.write_vn(out_vn, result)
    }

    /// New: JVM object allocation.  Opaque.
    pub(super) fn handle_new(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        let args = self.read_vns(&insn.inputs)?;
        let out_vn = require_output_vn(insn)?;
        let result = self.builder.build_new(&args, out_vn.int_type()?)?;
        self.write_vn(out_vn, result)
    }
}
