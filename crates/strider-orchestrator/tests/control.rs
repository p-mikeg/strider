//! Branches, loops, and merge points.
//!
//! 9 functions × 6 archs = 54 tests.

#![allow(
    clippy::panic,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::unreachable
)]

mod common;
use common::*;

per_arch_test!("control", "abs_val", abs_has_one_if);
per_arch_test!("control", "max_val", max_has_one_if);
per_arch_test!("control", "clamp", clamp_has_two_ifs);
per_arch_test!("control", "select_three", select_three_has_two_ifs);
per_arch_test!("control", "sum_to_n", sum_to_n_has_loop);
per_arch_test!("control", "factorial", factorial_has_loop);
per_arch_test!("control", "count_bits", count_bits_has_loop_and_shr);
// nested_loops: arm's `pop {pc}` resolves via the indirect-branch
// resolver's `LinkRegister` arm once `LoadForward` simplifies the
// loaded target back to `InitialVar(lr)`.
per_arch_test!("control", "nested_loops", nested_loops_has_two_loops);
// early_return uses count_return_paths (sum of Region fan-in at each
// Return) instead of bare Return-node count. PPC + aarch64be share the
// function epilogue at `-O0` (one `blr`/`ret` for all source-level
// returns), so they have a single Return fed by a 2-input Region;
// `count_return_paths` reports 2 there, matching the 2 separate Return
// nodes on x86/MIPS/ARM.
per_arch_test!(
    "control",
    "early_return",
    early_return_has_loop_and_two_returns
);

fn abs_has_one_if(function: &strider_ir::Function) {
    assert!(count_ifs(function) >= 1, "abs_val must have ≥1 If");
}
fn max_has_one_if(function: &strider_ir::Function) {
    assert!(count_ifs(function) >= 1, "max_val must have ≥1 If");
}
fn clamp_has_two_ifs(function: &strider_ir::Function) {
    assert!(
        count_ifs(function) >= 2,
        "clamp has 2 conditionals; got {}",
        count_ifs(function)
    );
}
fn select_three_has_two_ifs(function: &strider_ir::Function) {
    assert!(
        count_ifs(function) >= 2,
        "select_three has 2 conditionals; got {}",
        count_ifs(function)
    );
}
fn sum_to_n_has_loop(function: &strider_ir::Function) {
    assert!(
        count_loops(function) >= 1,
        "sum_to_n loop header missing VarPhi"
    );
}
fn factorial_has_loop(function: &strider_ir::Function) {
    assert!(
        count_loops(function) >= 1,
        "factorial loop header missing VarPhi"
    );
}
fn count_bits_has_loop_and_shr(function: &strider_ir::Function) {
    assert!(count_loops(function) >= 1);
    assert!(
        count_int_binop(function, strider_ir::IntBinaryOp::ShiftRight) >= 1,
        "count_bits has x>>=1"
    );
}
fn nested_loops_has_two_loops(function: &strider_ir::Function) {
    assert!(
        count_loops(function) >= 2,
        "nested_loops expected ≥2 VarPhi; got {}",
        count_loops(function)
    );
}
fn early_return_has_loop_and_two_returns(function: &strider_ir::Function) {
    assert!(count_loops(function) >= 1);
    assert!(
        count_return_paths(function) >= 2,
        "early_return has 2 source-level return paths; got {}",
        count_return_paths(function)
    );
}
