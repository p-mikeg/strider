//! Branches, loops, and merge points.
//!
//! 9 functions × 6 archs = 54 tests.

#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used, clippy::unreachable)]

mod common;
use common::*;

per_arch_test!("control", "abs_val",        abs_has_one_if);
per_arch_test!("control", "max_val",        max_has_one_if);
per_arch_test!("control", "clamp",          clamp_has_two_ifs);
per_arch_test!("control", "select_three",   select_three_has_two_ifs);
per_arch_test!("control", "sum_to_n",       sum_to_n_has_loop);
per_arch_test!("control", "factorial",      factorial_has_loop);
per_arch_test!("control", "count_bits",     count_bits_has_loop_and_shr);
// nested_loops: arm's `pop {pc}` resolves via the indirect-branch
// resolver's `LinkRegister` arm once `StackLoadForward` simplifies the
// loaded target back to `InitialVar(lr)`.
per_arch_test!("control", "nested_loops", nested_loops_has_two_loops);
// early_return uses count_return_paths (sum of ControlState fan-in at
// each Return) instead of bare Return-node count.  PPC + aarch64be
// share the function epilogue at `-O0` (one `blr`/`ret` for all
// source-level returns) so they have a single Return fed by a 2-input
// ControlState — `count_return_paths` reports 2 there, matching the 2
// separate Return nodes on x86/MIPS/ARM.
per_arch_test!("control", "early_return",   early_return_has_loop_and_two_returns);

fn abs_has_one_if(g: &strider_ir::Graph) {
    assert!(count_ifs(g) >= 1, "abs_val must have ≥1 If");
}
fn max_has_one_if(g: &strider_ir::Graph) {
    assert!(count_ifs(g) >= 1, "max_val must have ≥1 If");
}
fn clamp_has_two_ifs(g: &strider_ir::Graph) {
    assert!(count_ifs(g) >= 2, "clamp has 2 conditionals; got {}", count_ifs(g));
}
fn select_three_has_two_ifs(g: &strider_ir::Graph) {
    assert!(count_ifs(g) >= 2, "select_three has 2 conditionals; got {}", count_ifs(g));
}
fn sum_to_n_has_loop(g: &strider_ir::Graph) {
    assert!(count_loops(g) >= 1, "sum_to_n loop header missing VarPhi");
}
fn factorial_has_loop(g: &strider_ir::Graph) {
    assert!(count_loops(g) >= 1, "factorial loop header missing VarPhi");
}
fn count_bits_has_loop_and_shr(g: &strider_ir::Graph) {
    assert!(count_loops(g) >= 1);
    assert!(count_int_binop(g, strider_ir::IntBinaryOp::ShiftRight) >= 1, "count_bits has x>>=1");
}
fn nested_loops_has_two_loops(g: &strider_ir::Graph) {
    assert!(count_loops(g) >= 2, "nested_loops expected ≥2 VarPhi; got {}", count_loops(g));
}
fn early_return_has_loop_and_two_returns(g: &strider_ir::Graph) {
    assert!(count_loops(g) >= 1);
    assert!(count_return_paths(g) >= 2,
            "early_return has 2 source-level return paths; got {}", count_return_paths(g));
}
