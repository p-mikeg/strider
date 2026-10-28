//! AArch64 instruction semantics end to end.
//!
//! One binary over `arch_aarch64/`, so the shared test tree compiles
//! once for the group instead of once per file.

#[macro_use]
mod common;

#[path = "arch_aarch64/aarch64_float_to_int_rounding.rs"]
mod aarch64_float_to_int_rounding;

#[path = "arch_aarch64/aarch64_rounding_mode_conversions.rs"]
mod aarch64_rounding_mode_conversions;

#[path = "arch_aarch64/aarch64_scalar_simd_write_zeroes_upper_lanes.rs"]
mod aarch64_scalar_simd_write_zeroes_upper_lanes;

#[path = "arch_aarch64/aarch64_w_sign_extending_load.rs"]
mod aarch64_w_sign_extending_load;
