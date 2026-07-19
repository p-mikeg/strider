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
/// `addr_off`.  Handlers that read `addr_off` directly (Subpiece's byte offset,
/// Extract/Insert's lsb and bit_count, the LOAD/STORE space id, CallOther's
/// user-op id, SegmentOp's op id) would silently mis-decode a non-CONST input,
/// producing structurally valid but semantically wrong IR.  GHIDRA always emits
/// CONST here; this catches a malformed `.sla` or a hand-built `Insn`.
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

/// P-code puts the target space in a CONST varnode at `inputs[0]` whose offset
/// is a pointer to a Sleigh `AddrSpace`.  Reading `.addr_space` gives `CONST`,
/// the ENCODING's space, not the target, hence this decode.
///
/// # Safety
///
/// `VnSpace::by_id` reinterprets `addr_off` as a raw `AddrSpace` pointer, so
/// the precondition is that pointer's validity, NOT the CONST tag.
/// `ensure_const_space` below is a structural gate only and establishes
/// nothing about the pointer.  This stays `pub(crate)` because every in-crate
/// caller passes an `Insn` from `Sleigh::lift_one`, which always emits a valid
/// space-pointer encoding.  Do not widen the visibility, and never call it
/// with a hand-built `Insn`.
pub(crate) fn decode_space_id(insn: &rsleigh::Insn) -> Result<rsleigh::VnSpace> {
    let space_id_vn = *nth_input_or_err(insn, 0)?;
    ensure_const_space(&space_id_vn, insn.opcode, "input 0")?;
    // SAFETY: the pcode comes from `Sleigh::lift_one`, so the space pointer is
    // valid.  See the precondition above.
    Ok(unsafe { rsleigh::VnSpace::by_id(space_id_vn) })
}
