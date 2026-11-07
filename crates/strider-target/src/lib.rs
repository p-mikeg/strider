#![cfg_attr(test, allow(clippy::type_complexity))]

pub(crate) mod arch;
pub mod call_other_abi;
pub mod calling_convention;
pub(crate) mod family;

pub use arch::{
    ArchNames, ArchPreset, ByteOrder, DecodeTables, Endianness, FlowCommit, SleighArch,
};
pub use call_other_abi::BuiltCallOtherAbi;
pub use calling_convention::{BuiltCallingConvention, CallingConvention, CcPreset, StackArgs};

pub type Result<T> = anyhow::Result<T>;
