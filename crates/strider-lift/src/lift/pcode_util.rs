pub(crate) type Result<T> = anyhow::Result<T>;

pub(crate) fn require_output_vn(insn: &rsleigh::Insn) -> Result<&rsleigh::Vn> {
    insn.output.as_ref().ok_or_else(|| {
        anyhow::anyhow!(
            "instruction has no output varnode for opcode {:?}",
            insn.opcode
        )
    })
}

pub(crate) fn nth_input_or_err(insn: &rsleigh::Insn, n: usize) -> Result<&rsleigh::Vn> {
    insn.inputs.get(n).ok_or_else(|| {
        anyhow::anyhow!(
            "opcode {:?} has too few inputs: expected at least {}, got {}",
            insn.opcode,
            n + 1,
            insn.inputs.len()
        )
    })
}

/// Sleigh encodes a literal as `addr_space == CONST` with the value in
/// `addr_off`.  A handler reading `addr_off` directly would silently mis-decode
/// a non-CONST input, producing structurally valid but semantically wrong IR.
/// GHIDRA always emits CONST here; this catches a malformed `.sla` or a
/// hand-built `Insn`.
pub(crate) fn ensure_const_space(
    vn: &rsleigh::Vn,
    opcode: rsleigh::Opcode,
    slot_label: &str,
) -> Result<()> {
    if vn.addr_space != rsleigh::VnSpace::CONST {
        anyhow::bail!(
            "opcode {opcode:?}: {slot_label} must be a CONST-space varnode \
             (got addr_space {:?}); Sleigh's contract requires this slot \
             to encode a literal value",
            vn.addr_space,
        );
    }
    Ok(())
}

impl<R: rsleigh::MemReader> super::Lifter<R> {
    /// P-code puts the target space in a CONST varnode at `inputs[0]` whose
    /// offset is the decoding engine's id for a Sleigh `AddrSpace`.  Reading
    /// `.addr_space` gives `CONST`, the ENCODING's space, not the target, hence
    /// this decode, through this engine's own space table.
    ///
    /// `Self::check_cfg_space_ids` rejects a `Cfg` holding an id outside that
    /// table before any lift, so the error here answers only a hand-built
    /// `Insn`.  Matching by id does not identify the space: a foreign id at an
    /// address this engine reused resolves to whichever space this engine has
    /// there.
    pub(crate) fn decode_space_id(&self, insn: &rsleigh::Insn) -> Result<rsleigh::VnSpace> {
        let vn = *nth_input_or_err(insn, 0)?;
        ensure_const_space(&vn, insn.opcode, "input 0")?;
        self.engine.space_ids().resolve(vn).ok_or_else(|| {
            anyhow::anyhow!(
                "{:?}: space id {:#x} names no space of this engine",
                insn.opcode,
                vn.addr_off
            )
        })
    }
}
