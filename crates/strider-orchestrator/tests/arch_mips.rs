//! MIPS instruction semantics end to end.
//!
//! One binary over `arch_mips/`, so the shared test tree compiles
//! once for the group instead of once per file.

#[macro_use]
mod common;

#[path = "arch_mips/mips64_variable_shift_counts.rs"]
mod mips64_variable_shift_counts;

#[path = "arch_mips/mips_float_to_int_rounding.rs"]
mod mips_float_to_int_rounding;

#[path = "arch_mips/mips_narrowing_rounding.rs"]
mod mips_narrowing_rounding;

#[path = "arch_mips/semantics_mips64_n64.rs"]
mod semantics_mips64_n64;

#[path = "arch_mips/mips_cop0.rs"]
mod mips_cop0;
