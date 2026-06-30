//! Pure pcode varnode/insn decoding utilities shared by the lifter.
//!
//! Free helpers used by the per-CFG `FunctionLifter` lifter
//! (which owns the actual value- and control-opcode handlers): the
//! checked input accessors and the LOAD/STORE space decoder.

/// `Result` alias.  Every fallible helper here returns this type.
pub type Result<T> = anyhow::Result<T>;

/// Common boilerplate: require the instruction to have an output varnode and
/// return a borrowed reference to it.
pub(crate) fn require_output_vn(insn: &rsleigh::Insn) -> Result<&rsleigh::Vn> {
    insn.output.as_ref().ok_or_else(|| {
        anyhow::anyhow!(
            "instruction has no output varnode for opcode {:?}",
            insn.opcode
        )
    })
}

/// Returns `&insn.inputs[n]` or a typed "too few inputs" error.
///
/// # Errors
/// Returns an error when `insn.inputs` has `<= n` elements.
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

/// Asserts that a varnode `vn` lives in CONST space.  Sleigh encodes the
/// "this is a literal constant value" varnode by setting `addr_space ==
/// CONST` with the constant in `addr_off`.  Several opcode handlers
/// (Subpiece's `byte_offset`, Extract/Insert's `lsb`/`bit_count`, PtrAdd's
/// `elem_size`, the LOAD/STORE space id, CallOther's user-op id, SegmentOp's
/// op id) read `vn.addr_off` directly as a literal value and would silently
/// mis-decode any non-CONST input.  This is a defensive structural guard:
/// GHIDRA's Sleigh emitter always produces CONST in these slots, but a
/// malformed `.sla` spec or a fuzzer-built `Insn` would otherwise produce a
/// structurally valid but semantically wrong IR shape.
///
/// # Errors
/// Returns an error when `vn` is not in CONST space.
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

/// Decodes the target address space of a p-code `LOAD` / `STORE`.
///
/// P-code encodes the target space as a CONST-space varnode at `inputs[0]`
/// whose offset is a pointer to a Sleigh `AddrSpace` object.  Reading
/// `.addr_space` directly yields `CONST` (the encoding's space), not the
/// actual target space — callers that care about the target must decode
/// via [`rsleigh::VnSpace::by_id`].
///
/// # Safety / precondition
///
/// `rsleigh::VnSpace::by_id` reinterprets `space_id_vn.addr_off` as a raw
/// pointer to a Sleigh `AddrSpace`.  The safety precondition is therefore
/// **caller-guaranteed validity of that space pointer** — NOT the CONST-space
/// tag.  The `ensure_const_space` check below is only a structural sanity
/// gate (it rejects the obviously-malformed case where the slot isn't even a
/// literal) and does NOT establish the pointer's validity.  This function is
/// `pub(crate)`: every in-crate caller (`handle_load` / `handle_store`)
/// passes only `Insn`s sourced from `rsleigh::Sleigh::lift_one`, which emits
/// LOAD/STORE with a valid space-pointer encoding.  Do not widen the
/// visibility or call this with a hand-built `Insn` whose input-0 offset is
/// not a real `AddrSpace` pointer.
///
/// # Errors
///
/// Returns an error when `insn.inputs` is empty or the input-0 varnode is
/// not in CONST space.
pub(crate) fn decode_space_id(insn: &rsleigh::Insn) -> Result<rsleigh::VnSpace> {
    let space_id_vn = *nth_input_or_err(insn, 0)?;
    ensure_const_space(&space_id_vn, insn.opcode, "input 0")?;
    // SAFETY: the space pointer is valid because the pcode comes from
    // `rsleigh::Sleigh::lift_one` (see the precondition above).  The CONST
    // tag check is a structural gate, not the safety condition.
    Ok(unsafe { rsleigh::VnSpace::by_id(space_id_vn) })
}
