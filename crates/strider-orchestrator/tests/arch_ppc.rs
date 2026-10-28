//! PowerPC instruction semantics end to end.
//!
//! One binary over `arch_ppc/`, so the shared test tree compiles
//! once for the group instead of once per file.

#[macro_use]
mod common;

#[path = "arch_ppc/powerpc_byte_reversed_load.rs"]
mod powerpc_byte_reversed_load;

#[path = "arch_ppc/powerpc_popcntw.rs"]
mod powerpc_popcntw;

#[path = "arch_ppc/powerpc_user_op_results.rs"]
mod powerpc_user_op_results;

#[path = "arch_ppc/ppc32_isel.rs"]
mod ppc32_isel;

#[path = "arch_ppc/ppc64_elfv1_opd_entry.rs"]
mod ppc64_elfv1_opd_entry;

#[path = "arch_ppc/ppc_float_to_int_rounding.rs"]
mod ppc_float_to_int_rounding;

#[path = "arch_ppc/ppc_fp_load_store.rs"]
mod ppc_fp_load_store;

#[path = "arch_ppc/ppc_fp_status.rs"]
mod ppc_fp_status;

#[path = "arch_ppc/ppc_fpscr_writes.rs"]
mod ppc_fpscr_writes;

#[path = "arch_ppc/ppc_fused_multiply_add.rs"]
mod ppc_fused_multiply_add;

#[path = "arch_ppc/ppc_vector_return.rs"]
mod ppc_vector_return;

#[path = "arch_ppc/ppc_vsx_and_rounding.rs"]
mod ppc_vsx_and_rounding;
