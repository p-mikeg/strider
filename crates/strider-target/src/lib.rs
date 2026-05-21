//! Target description: architecture descriptors and calling conventions.
//!
//! Owns the pure data that describes the machine and ABI being analysed.

pub mod arch;
pub mod calling_convention;
pub mod call_other_abi;

pub use arch::{ArchPreset, Endianness, SleighArch};
pub use calling_convention::{BuiltCallingConvention, CallingConvention};

/// Crate-level `Result` alias.  Every fallible function in `strider-target`
/// returns this type.
pub type Result<T> = anyhow::Result<T>;
