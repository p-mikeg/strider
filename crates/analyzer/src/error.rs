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

    /// A test assertion failed. Exists so tests can return `Result<(), Error>`
    /// instead of using `panic!`.
    #[error("assertion failed: {0}")]
    AssertionFailed(String),
}

strider_error::define_error! {
    pub struct Error wraps ErrorKind;
    sources: [rsleigh::error::BaseError];
}

// Preserves origin backtrace + location chain across each crossing.
strider_error::bridge_error!(cfg::Error    => Error, ErrorKind::CfgError);
strider_error::bridge_error!(ir::Error     => Error, ErrorKind::IrError);
strider_error::bridge_error!(opt::Error    => Error, ErrorKind::OptError);
strider_error::bridge_error!(target::Error => Error, ErrorKind::TargetError);

/// the result type using our error.
pub type Result<T> = std::result::Result<T, Error>;
