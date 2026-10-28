//! x86, x86-64, x87 and SSE instruction semantics end to end.
//!
//! One binary over `arch_x86/`, so the shared test tree compiles
//! once for the group instead of once per file.

#[macro_use]
mod common;

#[path = "arch_x86/i386_bcd_adjust_and_arpl.rs"]
mod i386_bcd_adjust_and_arpl;

#[path = "arch_x86/semantics_x86_simd_shift_counts.rs"]
mod semantics_x86_simd_shift_counts;

#[path = "arch_x86/semantics_x87_extended_precision.rs"]
mod semantics_x87_extended_precision;

#[path = "arch_x86/sse_rounding_control.rs"]
mod sse_rounding_control;

#[path = "arch_x86/x86_64_shadow_stack_pointer.rs"]
mod x86_64_shadow_stack_pointer;

#[path = "arch_x86/x86_64_upper_half_is_cleared.rs"]
mod x86_64_upper_half_is_cleared;

#[path = "arch_x86/x86_conversion_rounding.rs"]
mod x86_conversion_rounding;

#[path = "arch_x86/x86_float_to_int_rounding.rs"]
mod x86_float_to_int_rounding;

#[path = "arch_x86/x86_high_byte_subregisters.rs"]
mod x86_high_byte_subregisters;

#[path = "arch_x86/x86_hlt_falls_through.rs"]
mod x86_hlt_falls_through;

#[path = "arch_x86/x86_int3_ends_the_path.rs"]
mod x86_int3_ends_the_path;

#[path = "arch_x86/x86_mmx_psubb_lanes.rs"]
mod x86_mmx_psubb_lanes;

#[path = "arch_x86/x86_shift_rotate_overflow.rs"]
mod x86_shift_rotate_overflow;

#[path = "arch_x86/x86_simd_lane_order.rs"]
mod x86_simd_lane_order;

#[path = "arch_x86/x87_fcmov_conditions.rs"]
mod x87_fcmov_conditions;

#[path = "arch_x86/x87_fprem_fxam_status.rs"]
mod x87_fprem_fxam_status;

#[path = "arch_x86/x87_fyl2x_lifts.rs"]
mod x87_fyl2x_lifts;

#[path = "arch_x86/x87_rounding_control.rs"]
mod x87_rounding_control;
