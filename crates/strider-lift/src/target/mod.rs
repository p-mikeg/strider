//! Target description: architecture descriptors and calling conventions.
//!
//! Absorbed from the standalone `target` crate. See
//! `docs/superpowers/plans/2026-05-17-strider-v2-rewrite.md` Phase 2
//! Task 2.2.
//!
//! Owns the pure data that describes the machine and ABI being analysed.

pub mod arch;
pub mod calling_convention;
pub mod call_other_abi;

pub use arch::{ArchContext, ArchPreset, Endianness, SleighArch};
pub use calling_convention::{
    BuiltCallingConvention, BuiltCallingConventionParts, CallingConvention,
};

/// Crate-level `Result` alias.  Every fallible function in `target` returns
/// this type.
pub type Result<T> = anyhow::Result<T>;
