//! Target description: architecture descriptors and calling conventions.
//!
//! Owns the pure data that describes the machine and ABI being analysed.
//!
//! Phase 2 Task 2.2 had originally moved this source into
//! `strider-lift`, but Phase 2 Task 2.3 needed strider-lift to also
//! depend on `opt` (for the cfg mini-IR resolver), and `opt -> target`
//! would have made a dependency cycle.  The resolution is to keep
//! `target` as a standalone crate and have `strider-lift::target`
//! re-export from here.  See
//! `docs/superpowers/plans/2026-05-17-strider-v2-rewrite.md` Phase 2
//! Task 2.3 cycle-break note.

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
