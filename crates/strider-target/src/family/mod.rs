mod aarch64;
mod arm32;
mod mips;
mod ppc;
#[cfg(test)]
mod tests;
mod x86;

pub(crate) use aarch64::AARCH64;
pub(crate) use arm32::ARM32;
pub(crate) use mips::MIPS;
pub(crate) use ppc::PPC;
pub(crate) use x86::X86;

/// Every family, in roster order.
pub(crate) static FAMILIES: [&Family; 5] = [&X86, &ARM32, &AARCH64, &MIPS, &PPC];

use crate::call_other_abi::{CallOtherClass, Prefix, Row};
use crate::calling_convention::CcPreset;

/// What strider knows about one architecture family, shared by its presets.
/// No `Default`: a new family answers every field.
pub(crate) struct Family {
    /// `"x86"`, `"arm32"`, `"aarch64"`, `"mips"` or `"ppc"`.
    #[cfg(any(test, feature = "test-util"))]
    pub(crate) name: &'static str,
    /// The Ghidra language definitions file this family's presets are rows
    /// of, under the vendored processors tree.
    #[cfg(any(test, feature = "test-util"))]
    pub(crate) ghidra_ldefs: &'static str,
    pub(crate) facts: DecodeFacts,
    /// The ops [`crate::call_other_abi::is_rounding_mode_control`] names.
    pub(crate) rounding_ops: &'static [&'static str],
    /// The op [`crate::call_other_abi::is_msr_register_mode`] names.
    pub(crate) msr_mode_op: Option<&'static str>,
    /// `(op, register)`: an input-less rounding op and the register it reads
    /// ([`crate::call_other_abi::rounding_mode_register`]).
    pub(crate) rounding_register: Option<(&'static str, &'static str)>,
    /// Exact rows, the first whose `only` admits the preset winning; consulted
    /// before the common rows and the stems ([`crate::call_other_abi::classify`]).
    pub(crate) call_other: &'static [Row],
    pub(crate) call_other_prefixes: &'static [Prefix],
    /// Classification by rule, for a family whose names no stem separates.
    pub(crate) call_other_fallback: Option<fn(&str) -> Option<CallOtherClass>>,
    /// This family's rows of [`crate::CallingConvention::presets`].
    pub(crate) calling_conventions: &'static [CcPreset],
}

/// What decoding and lifting know about one family, each undocumented field
/// the answer of the [`SleighArch`](crate::SleighArch) method of the same
/// name. No `Default`: a new family answers every field.
pub(crate) struct DecodeFacts {
    pub(crate) isa_mode_var: Option<&'static str>,
    pub(crate) flow_commit_vars: &'static [&'static str],
    pub(crate) transient_decode_vars: &'static [&'static str],
    pub(crate) has_delay_slots: bool,
    /// [`SleighArch::callother_output_is_exact_in`](crate::SleighArch::callother_output_is_exact_in)
    /// short of its x87 rule.
    pub(crate) callother_output_exact: bool,
    pub(crate) register_space_reach: Option<core::ops::Range<u64>>,
    pub(crate) internal_registers: &'static [&'static str],
    /// Volatile registers the pspec does not mark.
    pub(crate) volatile_registers: &'static [&'static str],
    /// Registers inside a pspec volatile range that are not volatile.
    pub(crate) non_volatile_registers: &'static [&'static str],
}
