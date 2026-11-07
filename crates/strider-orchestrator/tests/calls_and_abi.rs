//! Calls, calling conventions and stack arguments.
//!
//! One binary over `calls_and_abi/`, so the shared test tree compiles
//! once for the group instead of once per file.

#[macro_use]
mod common;

#[path = "calls_and_abi/bounded_lift_tail_call.rs"]
mod bounded_lift_tail_call;

#[path = "calls_and_abi/call_other_precise_abi.rs"]
mod call_other_precise_abi;

#[path = "calls_and_abi/call_other_simd_abi.rs"]
mod call_other_simd_abi;

#[path = "calls_and_abi/call_stack_args_merged_store.rs"]
mod call_stack_args_merged_store;

#[path = "calls_and_abi/call_stack_args_patched_slot.rs"]
mod call_stack_args_patched_slot;

#[path = "calls_and_abi/call_stack_effects.rs"]
mod call_stack_effects;

#[path = "calls_and_abi/calling_convention.rs"]
mod calling_convention;

#[path = "calls_and_abi/float_call_args.rs"]
mod float_call_args;

#[path = "calls_and_abi/float_function_args.rs"]
mod float_function_args;

#[path = "calls_and_abi/frame_address_in_call_register.rs"]
mod frame_address_in_call_register;

#[path = "calls_and_abi/no_return_targets.rs"]
mod no_return_targets;

#[path = "calls_and_abi/per_address_cc.rs"]
mod per_address_cc;

#[path = "calls_and_abi/per_address_cc_indirect.rs"]
mod per_address_cc_indirect;
