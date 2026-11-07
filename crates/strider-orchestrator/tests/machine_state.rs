//! Registers, register-space memory and stack slots.
//!
//! One binary over `machine_state/`, so the shared test tree compiles
//! once for the group instead of once per file.

#[macro_use]
mod common;

#[path = "machine_state/entry_self_loop_phi.rs"]
mod entry_self_loop_phi;

#[path = "machine_state/known_bits_e2e.rs"]
mod known_bits_e2e;

#[path = "machine_state/read_reg_vn_truncate.rs"]
mod read_reg_vn_truncate;

#[path = "machine_state/register_space_byte_order.rs"]
mod register_space_byte_order;

#[path = "machine_state/register_space_store_writes_a_register.rs"]
mod register_space_store_writes_a_register;

#[path = "machine_state/register_space_unresolvable_is_opaque.rs"]
mod register_space_unresolvable_is_opaque;

#[path = "machine_state/rounding_mode_ops.rs"]
mod rounding_mode_ops;

#[path = "machine_state/stack_offset_detect_e2e.rs"]
mod stack_offset_detect_e2e;

#[path = "machine_state/subregister_snippet_e2e.rs"]
mod subregister_snippet_e2e;

#[path = "machine_state/volatile_register_reads.rs"]
mod volatile_register_reads;
