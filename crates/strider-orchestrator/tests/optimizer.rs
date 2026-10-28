//! Optimizer pipeline behaviour over lifted fixtures.
//!
//! One binary over `optimizer/`, so the shared test tree compiles
//! once for the group instead of once per file.

#[macro_use]
mod common;

#[path = "optimizer/asm_fingerprint_propagation.rs"]
mod asm_fingerprint_propagation;

#[path = "optimizer/asm_fingerprints.rs"]
mod asm_fingerprints;

#[path = "optimizer/build_ir_with_overrides.rs"]
mod build_ir_with_overrides;

#[path = "optimizer/factor_mul_and_shift.rs"]
mod factor_mul_and_shift;

#[path = "optimizer/flag_cmp.rs"]
mod flag_cmp;

#[path = "optimizer/flag_cmp_immediate.rs"]
mod flag_cmp_immediate;

#[path = "optimizer/frint_is_not_a_float_to_int.rs"]
mod frint_is_not_a_float_to_int;

#[path = "optimizer/graph_rewriter.rs"]
mod graph_rewriter;

#[path = "optimizer/multi_pass_cooperation.rs"]
mod multi_pass_cooperation;

#[path = "optimizer/semantics_raw_versus_optimized.rs"]
mod semantics_raw_versus_optimized;
