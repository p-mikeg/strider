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
pub mod user_ops;

pub use arch::{Endianness, SleighArch};
pub use calling_convention::{BuiltCallingConvention, CallingConvention};

/// Crate-level `Result` alias.  Every fallible function in `target` returns
/// this type.
pub type Result<T> = anyhow::Result<T>;
