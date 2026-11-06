//! ARM / Thumb / BE8 instruction semantics end to end.
//!
//! One binary over `arch_arm/`, so the shared test tree compiles
//! once for the group instead of once per file.

#[macro_use]
mod common;

#[path = "arch_arm/arm_cpsr_state.rs"]
mod arm_cpsr_state;

#[path = "arch_arm/arm_mode_switch_rebanks.rs"]
mod arm_mode_switch_rebanks;

#[path = "arch_arm/arm_neon_lane_insert.rs"]
mod arm_neon_lane_insert;

#[path = "arch_arm/arm_neon_vld1_vst1_element_order.rs"]
mod arm_neon_vld1_vst1_element_order;

#[path = "arch_arm/arm_neon_vtbl.rs"]
mod arm_neon_vtbl;

#[path = "arch_arm/arm_smc_hvc_lift.rs"]
mod arm_smc_hvc_lift;

#[path = "arch_arm/arm_vcvt_rounding.rs"]
mod arm_vcvt_rounding;

#[path = "arch_arm/arm_vfp_narrowing_rounding.rs"]
mod arm_vfp_narrowing_rounding;

#[path = "arch_arm/be8_end_to_end.rs"]
mod be8_end_to_end;

#[path = "arch_arm/be8_register_slicing.rs"]
mod be8_register_slicing;

#[path = "arch_arm/thumb2_logical_carry.rs"]
mod thumb2_logical_carry;
