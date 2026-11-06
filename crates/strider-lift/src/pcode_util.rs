use anyhow::Result;

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

/// Rejects a non-CONST varnode in a slot Sleigh fills with a literal, which
/// only a malformed `.sla` or a hand-built `Insn` produces.
pub(crate) fn ensure_const_space(
    vn: &rsleigh::Vn,
    opcode: rsleigh::Opcode,
    slot_label: &str,
) -> Result<()> {
    if vn.addr_space != rsleigh::VnSpace::CONST {
        anyhow::bail!(
            "opcode {opcode:?}: {slot_label} is not a CONST varnode (got {:?})",
            vn.addr_space,
        );
    }
    Ok(())
}

impl<R: rsleigh::MemReader> super::Lifter<R> {
    /// The target space of a LOAD/STORE: `inputs[0]` is a CONST varnode whose
    /// offset is this engine's space id. `Self::check_cfg_space_ids` rejects
    /// unknown ids before any lift, so the error answers a hand-built `Insn`.
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
