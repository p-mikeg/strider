// Test code uses idiomatic .unwrap() / .expect() / panic! / unreachable! /
// assert! macros which the workspace's production-code lints would otherwise
// reject.  Production code is held to the strict denied set; the cfg_attr
// below opts test compilation out of those specific lints only.
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::todo,
        clippy::type_complexity,
    )
)]

//! Target description: architecture descriptors and calling conventions.
//!
//! Owns the pure data that describes the machine and ABI being analysed.

pub mod arch;
pub mod call_descriptor;
pub mod call_other_abi;
pub mod calling_convention;

pub use arch::{ArchPreset, Endianness, SleighArch};
pub use call_descriptor::CallDescriptor;
pub use call_other_abi::BuiltCallOtherAbi;
pub use calling_convention::{
    BuiltCallingConvention, CallingConvention, MissingPresetError, StackArgs,
};

/// Crate-level `Result` alias.  Every fallible function in `strider-target`
/// returns this type.
pub type Result<T> = anyhow::Result<T>;
