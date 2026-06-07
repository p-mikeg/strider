//! Pure pcode varnode/insn decoding utilities shared by the lifter.
//!
//! Free helpers used by the per-CFG [`super::FunctionLifter`] lifter
//! (which owns the actual value- and control-opcode handlers): the
//! deterministic varnode sort key, the checked input accessors, and the
//! LOAD/STORE space decoder.

/// `Result` alias.  Every fallible helper here returns this type.
pub type Result<T> = anyhow::Result<T>;

/// Common boilerplate: require the instruction to have an output varnode and
/// return a borrowed reference to it.
pub(crate) fn require_output_vn(insn: &rsleigh::Insn) -> Result<&rsleigh::Vn> {
    insn.output
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("instruction has no output varnode for opcode {:?}", insn.opcode))
}

/// Stable sort key for varnodes.  Used by the strider lifting path to
/// give `FunctionBuilder` the same VarId numbering across runs (a
/// `HashSet` iteration order would otherwise depend on the random hasher
/// seed).
pub fn vn_sort_key(vn: &rsleigh::Vn) -> (u8, u64, u32) {
    (vn.addr_space.shortcut_raw(), vn.addr_off, vn.size)
}

/// Returns `insn.inputs[0]` or a typed "too few inputs" error.  Used by
/// LOAD/STORE space decoding and any other opcode that requires a
/// distinguished varnode at slot 0.
///
/// # Errors
///
/// Returns an error when `insn.inputs` is empty.
pub fn first_input_or_err(insn: &rsleigh::Insn) -> Result<&rsleigh::Vn> {
    insn.inputs.first().ok_or_else(|| {
        anyhow::anyhow!(
            "opcode {:?} has too few inputs: expected at least 1, got 0",
            insn.opcode
        )
    })
}

/// Returns `&insn.inputs[n]` or a typed "too few inputs" error.
///
/// # Errors
/// Returns an error when `insn.inputs` has `<= n` elements.
pub fn nth_input_or_err(insn: &rsleigh::Insn, n: usize) -> Result<&rsleigh::Vn> {
    insn.inputs.get(n).ok_or_else(|| {
        anyhow::anyhow!(
            "opcode {:?} has too few inputs: expected at least {}, got {}",
            insn.opcode,
            n + 1,
            insn.inputs.len()
        )
    })
}

/// Decodes the target address space of a p-code `LOAD` / `STORE`.
///
/// P-code encodes the target space as a CONST-space varnode at `inputs[0]`
/// whose offset is a pointer to a Sleigh `AddrSpace` object.  Reading
/// `.addr_space` directly yields `CONST` (the encoding's space), not the
/// actual target space — callers that care about the target must decode
/// via [`rsleigh::VnSpace::by_id`].
///
/// # Errors
///
/// Returns an error when `insn.inputs` is empty or the input-0 varnode is
/// not in CONST space.
pub fn decode_space_id(insn: &rsleigh::Insn) -> Result<rsleigh::VnSpace> {
    let space_id_vn = *first_input_or_err(insn)?;
    if space_id_vn.addr_space != rsleigh::VnSpace::CONST {
        anyhow::bail!(
            "opcode {:?} expects a CONST input at position 0",
            insn.opcode
        );
    }
    // SAFETY: `VnSpace::by_id`'s precondition is that `space_id_vn`'s
    // offset is a valid pointer to a Sleigh `AddrSpace`.  This holds
    // because the pcode comes from `rsleigh::Sleigh::lift_one`, which
    // only emits LOAD/STORE with a valid space-pointer encoding.  The
    // CONST-space tag check above is a structural sanity gate, not the
    // safety condition itself.
    Ok(unsafe { rsleigh::VnSpace::by_id(space_id_vn) })
}
