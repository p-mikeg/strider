#![cfg_attr(
    test,
    allow(
        clippy::panic,
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::unreachable
    )
)]

//! Target description: architecture descriptors and calling conventions.
//!
//! This crate owns the pure data that describes the machine and ABI being
//! analysed.  It sits below `ir`, `opt`, and `strider` so every layer that
//! needs ABI information can name the same types.

pub mod arch;
pub mod calling_convention;
pub mod error;

pub use arch::{Endianness, SleighArch};
pub use calling_convention::{BuiltCallingConvention, CallingConvention};
pub use error::{Error, ErrorKind, Result};
