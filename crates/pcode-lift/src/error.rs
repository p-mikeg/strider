//! Error type for the `pcode-lift` crate.
//!
//! Mirrors the shape of `strider::error` (a [`thiserror`] enum behind a
//! `define_error!` wrapper).  The variants are a strict subset of the
//! strider error: the value-producing handlers cannot raise CFG- or
//! optimizer-related errors, so those are not wrapped.

#[derive(Debug, thiserror::Error)]
pub enum ErrorKind {
    #[error(transparent)]
    SleighError(#[from] rsleigh::error::BaseError),

    #[error(transparent)]
    IrError(ir::ErrorKind),

    #[error("instruction has no output varnode for opcode {0:?}")]
    MissingOutputVn(rsleigh::Opcode),

    #[error("attempted to write to CONST space: {0:?}")]
    WriteToConstSpace(rsleigh::VnSpace),

    #[error("unsupported varnode space {0:?}")]
    UnsupportedVnSpace(rsleigh::VnSpace),

    #[error("unsupported register size {0} bytes")]
    UnsupportedRegSize(u32),

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

    #[error("register {0:?} has no enclosing container in variable set")]
    NoRegisterContainer(rsleigh::Vn),
}

strider_error::define_error! {
    pub struct Error wraps ErrorKind;
    sources: [rsleigh::error::BaseError];
}

// Bridge ir::Error so per-handler `?` works on builder calls.
strider_error::bridge_error!(ir::Error => Error, ErrorKind::IrError);

/// Result alias used inside `pcode-lift`.
pub type Result<T> = std::result::Result<T, Error>;
