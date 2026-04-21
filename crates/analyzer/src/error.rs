strider_error::define_error! {
    pub struct Error wraps ErrorKind;
    sources: [rsleigh::error::BaseError];

    #[derive(Debug, thiserror::Error)]
    pub enum ErrorKind {
        #[error(transparent)]
        SleighError(#[from] rsleigh::error::BaseError),

        #[error("generic sleigh error {0:?}")]
        GenericSleighError(String),

        #[error("unknown register name by sleign {0:?}")]
        UnknownRegName(String),

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
}

/// Hand-rolled bridge from `cfg::Error`. Preserves the origin backtrace +
/// location chain captured by `cfg` and appends this crossing site.
impl From<cfg::Error> for Error {
    #[track_caller]
    fn from(e: cfg::Error) -> Self {
        let (kind, fields) = e.decompose();
        Error {
            kind: Box::new(ErrorKind::CfgError(*kind)),
            fields: fields.push_caller(),
        }
    }
}

/// Hand-rolled bridge from `ir::Error`.
impl From<ir::Error> for Error {
    #[track_caller]
    fn from(e: ir::Error) -> Self {
        let (kind, fields) = e.decompose();
        Error {
            kind: Box::new(ErrorKind::IrError(*kind)),
            fields: fields.push_caller(),
        }
    }
}

/// Hand-rolled bridge from `opt::Error`.
impl From<opt::Error> for Error {
    #[track_caller]
    fn from(e: opt::Error) -> Self {
        let (kind, fields) = e.decompose();
        Error {
            kind: Box::new(ErrorKind::OptError(*kind)),
            fields: fields.push_caller(),
        }
    }
}

/// the result type using our error.
pub type Result<T> = std::result::Result<T, Error>;
