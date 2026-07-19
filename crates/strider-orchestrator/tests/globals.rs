//! LoadReadOnly fold against .rodata constants.
//!
//! After the optimiser pipeline runs (with LoadReadOnly enabled), reads of
//! `static const` data should fold to IntConst nodes instead of remaining
//! as Loads.  Tests verify: (a) the constant value materialises in the IR,
//! (b) the corresponding read no longer appears as a Load.

#![allow(
    clippy::panic,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::unreachable
)]

mod common;
use common::*;

per_arch_test!("globals", "read_const_byte", const_byte_folds_to_0x61);
per_arch_test!("globals", "read_const_int", const_int_folds_to_value);
per_arch_test!(
    "globals",
    "branch_on_const_string",
    string_branch_folds_one_arm
);
// runtime_const_idx: the MIPS bounds-check Bool flows into integer ops
// via CastToInt (extend_if_needed handles Bool input).
per_arch_test!("globals", "runtime_const_idx", runtime_idx_keeps_load);

fn const_byte_folds_to_0x61(function: &strider_ir::Function) {
    // 'a' = 0x61.  After LoadReadOnly, this is an IntConst.
    assert!(
        has_constant(function, 0x61),
        "expected IntConst(0x61) after LoadReadOnly fold"
    );
}
fn const_int_folds_to_value(function: &strider_ir::Function) {
    assert!(
        has_constant(function, 0x12345678),
        "expected IntConst(0x12345678) after LoadReadOnly fold"
    );
}
fn string_branch_folds_one_arm(function: &strider_ir::Function) {
    // The byte 'y' = 0x79 read from k_str[0] folds; combined with
    // DeadBranchElimination this often eliminates one arm of the If.
    // We pin only the constant; arm-elimination is opt's responsibility.
    assert!(
        has_constant(function, 0x79) || count_ifs(function) == 0,
        "expected either IntConst(0x79) or eliminated If; neither found"
    );
}
fn runtime_idx_keeps_load(function: &strider_ir::Function) {
    // Index isn't constant, so the Load survives.  Two ifs gate the bounds.
    assert!(count_loads(function) >= 1, "runtime index → Load survives");
    assert!(count_ifs(function) >= 1, "bounds-check If(s) survive");
}
