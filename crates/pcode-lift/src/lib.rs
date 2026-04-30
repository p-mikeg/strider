#![cfg_attr(
    test,
    allow(
        clippy::panic,
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::unreachable
    )
)]

//! Pure value-producing pcode → IR lifter, factored out of `strider`.
//!
//! The [`ValueLifter`] owns the per-opcode handlers for every pcode opcode
//! that produces a value (arithmetic, integer, float, boolean, casts,
//! memory loads, miscellaneous value ops).  Control-flow opcodes (Branch,
//! CondBranch, Return, BranchIndirect, Call, CallIndirect, Store) are NOT
//! handled here — [`ValueLifter::lift`] returns `Ok(false)` so the caller
//! can route them through its own (region-aware) machinery.
//!
//! This separation lets two callers reuse the value-lifting logic:
//!
//! * `strider`, which uses it as the inner-loop dispatch for translating
//!   a CFG region into the per-region IR;
//! * `cfg`, which uses it (planned) to build a stand-alone single-block
//!   mini-IR for resolving the targets of indirect branches.

pub mod value;
pub mod vn_io;

/// Crate-level `Result` alias.  Every fallible function in `pcode_lift`
/// returns this type.
pub type Result<T> = anyhow::Result<T>;

/// Lifts a single value-producing pcode instruction into IR nodes.
///
/// Holds borrows to the IR [`FunctionBuilder`] being filled in, the
/// [`rsleigh::Sleigh`] context (for address-space / register metadata),
/// and the target architecture's endianness (used by the register
/// aliasing logic in [`vn_io`]).
///
/// Construct one per pcode insn (or per region — the lifter is
/// stateless beyond the borrows it carries).
///
/// [`FunctionBuilder`]: ir::FunctionBuilder
pub struct ValueLifter<'a, R: rsleigh::MemReader> {
    /// IR builder receiving the lifted nodes.
    pub builder: &'a mut ir::FunctionBuilder,
    /// Sleigh context for the source binary.  Needed to decode address
    /// spaces (e.g. CONST, REGISTER, default-code space) when reading
    /// and writing varnodes.
    pub sleigh: &'a rsleigh::Sleigh<R>,
    /// Target endianness — drives the bit-shift formula used when
    /// reading or writing a sub-register inside a wider container.
    pub endianness: target::Endianness,
}

impl<'a, R: rsleigh::MemReader> ValueLifter<'a, R> {
    /// Creates a new [`ValueLifter`] borrowing the given builder, sleigh
    /// context, and endianness.
    pub fn new(
        builder: &'a mut ir::FunctionBuilder,
        sleigh: &'a rsleigh::Sleigh<R>,
        endianness: target::Endianness,
    ) -> Self {
        Self {
            builder,
            sleigh,
            endianness,
        }
    }

    /// Lifts a single pcode instruction.
    ///
    /// Returns `Ok(true)` when `insn`'s opcode is value-producing and
    /// was lifted into IR nodes.  Returns `Ok(false)` when the opcode
    /// is a control-flow / call / store op that the caller is
    /// responsible for handling — the caller observes the `false` and
    /// dispatches via its own machinery.
    ///
    /// # Errors
    ///
    /// Returns an error when the instruction is malformed (missing
    /// output varnode, wrong number of inputs, unsupported sizes,
    /// etc.) or when an underlying IR builder call fails.
    pub fn lift(&mut self, insn: &rsleigh::Insn) -> Result<bool> {
        value::lift(self, insn)
    }
}

/// Common boilerplate: require the instruction to have an output varnode and
/// return a borrowed reference to it.
pub(crate) fn require_output_vn(insn: &rsleigh::Insn) -> Result<&rsleigh::Vn> {
    insn.output
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("instruction has no output varnode for opcode {:?}", insn.opcode))
}

/// Stable sort key for varnodes.  Used by the cfg + strider lifting paths
/// to give `FunctionBuilder` the same VarId numbering across runs (a
/// `HashSet` iteration order would otherwise depend on the random hasher
/// seed).  Both lifters key off this same order so downstream IRs that
/// share VarIds (e.g. mini-IR for indirect-branch resolution and the
/// final per-region IR) stay aligned.
#[must_use]
pub fn vn_sort_key(vn: &rsleigh::Vn) -> (u8, u64, u32) {
    (vn.addr.space.shortcut_raw(), vn.addr.off, vn.size)
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

/// Decodes the target address space of a p-code `LOAD` / `STORE`.
///
/// P-code encodes the target space as a CONST-space varnode at `inputs[0]`
/// whose offset is a pointer to a Sleigh `AddrSpace` object.  Reading
/// `.addr.space` directly yields `CONST` (the encoding's space), not the
/// actual target space — callers that care about the target must decode
/// via [`rsleigh::VnSpace::by_id`].
///
/// # Errors
///
/// Returns an error when `insn.inputs` is empty or the input-0 varnode is
/// not in CONST space.
pub fn decode_space_id(insn: &rsleigh::Insn) -> Result<rsleigh::VnSpace> {
    let space_id_vn = *first_input_or_err(insn)?;
    if space_id_vn.addr.space != rsleigh::VnSpace::CONST {
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
