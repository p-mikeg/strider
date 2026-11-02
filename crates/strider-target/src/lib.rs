#![cfg_attr(test, allow(clippy::type_complexity))]

pub(crate) mod arch;
pub(crate) mod arch_names;
pub mod call_other_abi;
pub mod calling_convention;
pub(crate) mod decode_tables;

pub(crate) use arch::ArchFamily;
pub use arch::{ArchPreset, ByteOrder, Endianness, SleighArch};
pub use arch_names::ArchNames;
pub use call_other_abi::BuiltCallOtherAbi;
pub use calling_convention::{BuiltCallingConvention, CallingConvention, StackArgs};
pub use decode_tables::{DecodeTables, FlowCommit};

pub type Result<T> = anyhow::Result<T>;
