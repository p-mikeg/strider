// Opts test compilation out of the workspace's strict production-code lints so
// tests can use unwrap/expect/panic idiomatically.  Production code keeps them.
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

pub mod arch;
pub mod call_other_abi;
pub mod calling_convention;

pub use arch::{ArchPreset, Endianness, SleighArch};
pub use call_other_abi::BuiltCallOtherAbi;
pub use calling_convention::{
    BuiltCallingConvention, BuiltCallingConventionParts, CallingConvention, StackArgs,
};

pub type Result<T> = anyhow::Result<T>;
