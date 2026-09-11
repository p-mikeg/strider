#![cfg_attr(test, allow(clippy::type_complexity))]

pub mod arch;
pub mod call_other_abi;
pub mod calling_convention;

pub use arch::{ArchPreset, Endianness, SleighArch};
pub use call_other_abi::BuiltCallOtherAbi;
pub use calling_convention::{BuiltCallingConvention, CallingConvention, StackArgs};

pub type Result<T> = anyhow::Result<T>;
