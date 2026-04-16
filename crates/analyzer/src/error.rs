use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error(transparent)]
    SleighError(#[from] rsleigh::error::BaseError),

    #[error("generic sleigh error {0:?}")]
    GenericSleighError(String),

    #[error("unknown register name by sleign {0:?}")]
    UnknownRegName(String),

    #[error("no region {0:?} in cfg")]
    CfgNoRegion(cfg::RegionId),

    #[error(transparent)]
    CfgError(#[from] cfg::Error),

    #[error(transparent)]
    IrError(#[from] ir::Error),

    #[error(transparent)]
    OptError(#[from] opt::Error),

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

    #[error("stack pointer register {0:?} must be listed in callee_saved_regs")]
    StackPtrNotCalleeSaved(&'static str),

    #[error("opcode {0:?} expects a CONST input at position {1}")]
    ExpectedConstInput(rsleigh::Opcode, usize),

    #[error("opcode {0:?} is decompiler-internal and should not appear in raw p-code")]
    UnexpectedDecompilerOpcode(rsleigh::Opcode),

    #[error("opcode {0:?} has too few inputs: expected at least {1}, got {2}")]
    TooFewInputs(rsleigh::Opcode, usize, usize),
}

/// the result type using our error.
pub type Result<T> = std::result::Result<T, Error>;
