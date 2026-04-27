#[derive(Debug, thiserror::Error)]
pub enum ErrorKind {
    #[error(transparent)]
    SleighError(#[from] rsleigh::error::BaseError),

    #[error("generic sleigh error {0:?}")]
    GenericSleighError(String),

    #[error(transparent)]
    TargetError(target::ErrorKind),

    #[error("no region {0:?} in cfg")]
    CfgNoRegion(cfg::RegionId),

    #[error(transparent)]
    CfgError(cfg::ErrorKind),

    #[error(transparent)]
    IrError(ir::ErrorKind),

    #[error(transparent)]
    OptError(opt::ErrorKind),

    #[error(transparent)]
    PcodeLiftError(pcode_lift::ErrorKind),

    #[error("register {0:?} has no enclosing container in variable set")]
    NoRegisterContainer(rsleigh::Vn),

    #[error("instruction has no output varnode for opcode {0:?}")]
    MissingOutputVn(rsleigh::Opcode),

    #[error("IR region not found for CFG region {0:?}")]
    IrRegionNotFound(cfg::RegionId),

    #[error("attempted to write to CONST space: {0:?}")]
    WriteToConstSpace(rsleigh::VnSpace),

    #[error("unsupported varnode space {0:?}")]
    UnsupportedVnSpace(rsleigh::VnSpace),

    #[error("unsupported register size {0} bytes")]
    UnsupportedRegSize(u32),

    #[error("unimplemented p-code opcode {0:?}")]
    UnimplementedOpcode(rsleigh::Opcode),

    #[error("unsupported float varnode size {0} bytes (expected 4 or 8)")]
    UnsupportedFloatSize(u32),

    #[error("opcode {0:?} expects a CONST input at position {1}")]
    ExpectedConstInput(rsleigh::Opcode, usize),

    #[error("opcode {0:?} is decompiler-internal and should not appear in raw p-code")]
    UnexpectedDecompilerOpcode(rsleigh::Opcode),

    #[error("opcode {0:?} has too few inputs: expected at least {1}, got {2}")]
    TooFewInputs(rsleigh::Opcode, usize, usize),

    #[error("Subpiece byte_offset {byte_offset} out of range for input size {input_size} (opcode {opcode:?})")]
    SubpieceOffsetOutOfRange {
        opcode: rsleigh::Opcode,
        byte_offset: u64,
        input_size: u32,
    },

    /// A test assertion failed. Exists so tests can return `Result<(), Error>`
    /// instead of using `panic!`.
    #[error("assertion failed: {0}")]
    AssertionFailed(String),

    /// Returned by tier-2 in-place editors and orchestrator helpers
    /// when given a node id that does not have the expected
    /// [`ir::node::NodeKind`].  Carries the offending node id and a
    /// human-readable name of the expected kind.
    #[error("node {node:?} does not have expected kind {expected}")]
    WrongNodeKind {
        node: ir::node::NodeId,
        expected: &'static str,
    },

    /// A code path that the round-1 indirect-branch fixed-point loop
    /// has not yet implemented.  Carries a description of the missing
    /// path so callers can surface a meaningful diagnostic.  Round-2+
    /// will replace these errors with real implementations.
    #[error("not yet implemented: {0}")]
    Unimplemented(String),

    /// Returned by the strider-level outer loop when it iterates more
    /// than its bounded cap (`2 * pending_at_iter_0 + 4`).  Hitting
    /// this error indicates a soundness bug in the resolver — every
    /// legal classification transition strictly grows the induced
    /// edge set, so the loop must terminate within the cap.  No panic.
    #[error("indirect-branch resolver did not converge after {0} iterations")]
    IndirectResolutionDidNotConverge(usize),

    /// Returned at fixed point if any `BranchIndirect` remains
    /// unresolved.  Carries the offending pcode address so callers
    /// can correlate with the disassembly.
    #[error("indirect branch at {0:?} could not be resolved at fixed point")]
    UnresolvedIndirectBranch(cfg::PcodeInsnAddr),
}

strider_error::define_error! {
    pub struct Error wraps ErrorKind;
    sources: [rsleigh::error::BaseError];
}

// Preserves origin backtrace + location chain across each crossing.
strider_error::bridge_error!(cfg::Error        => Error, ErrorKind::CfgError);
strider_error::bridge_error!(ir::Error         => Error, ErrorKind::IrError);
strider_error::bridge_error!(opt::Error        => Error, ErrorKind::OptError);
strider_error::bridge_error!(target::Error     => Error, ErrorKind::TargetError);
strider_error::bridge_error!(pcode_lift::Error => Error, ErrorKind::PcodeLiftError);

/// the result type using our error.
pub type Result<T> = std::result::Result<T, Error>;
