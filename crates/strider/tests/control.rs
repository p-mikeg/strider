//! Branches, loops, and merge points.
//!
//! 9 functions × 6 archs = 54 tests.

#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used, clippy::unreachable)]

mod common;
use common::*;

per_arch_test!("control", "abs_val",        abs_has_one_if);
// max_val: BUG-2 (MIPS comparison CFG) is fixed; this is the regression test.
per_arch_test!("control", "max_val",        max_has_one_if);
// clamp / factorial / early_return: BUG-3 (Bool->AnyInt via extend_if_needed)
// is fixed; these are the regression coverage.
per_arch_test!("control", "clamp",          clamp_has_two_ifs);
// select_three: BUG-4 (ARM conditional select non-Bool) is fixed by the
// BUG-3 coerce-on-write at write_reg_vn — same Bool-flag-via-1-byte-reg
// chain.
per_arch_test!("control", "select_three",   select_three_has_two_ifs);
per_arch_test!("control", "sum_to_n",       sum_to_n_has_loop);
per_arch_test!("control", "factorial",      factorial_has_loop);
per_arch_test!("control", "count_bits",     count_bits_has_loop_and_shr);
// nested_loops: pre-Phase-5 BUG-5 fix collapsed BranchIndirect into
// Return.  Phase 5's resolver replaces that mapping with a real
// constant-target prover; ARM single-register `pop {pc}` lifts to
// `load tmp = [sp]; BranchIndirect tmp`, which the resolver cannot
// prove without stack-load forwarding.  Same root cause as
// `tail_caller::arm` — see `crates/strider/tests/abi.rs`.  Tracked
// under BUG-5.
per_arch_test!(
    "control", "nested_loops", nested_loops_has_two_loops,
    ignore = {
        Arm: "BUG-5 residue: arm `pop {pc}` lifts to load+BranchIndirect; resolver lacks stack-load-forward",
    }
);
// early_return: BUG-3 post-opt residue fixed by:
//   1. write_reg_vn coercing val to reg's declared int type at the simple
//      `container == reg` write path (so 1-byte flag registers don't smuggle
//      Bool through as the variable's bound value), AND
//   2. handle_cond_branch coercing the read condition back to Bool before
//      handing it to build_if (which only accepts Bool).
// BUG-22 fixed: count *control paths into Return* (sum of ControlState fan-in
// at each Return) instead of bare Return-node count.  PPC + aarch64be share
// the function epilogue at `-O0` (one `blr`/`ret` for all source-level returns)
// so they have a single Return fed by a 2-input ControlState — `count_return_paths`
// reports 2 there, matching the 2 separate Return nodes on x86/MIPS/ARM.
per_arch_test!("control", "early_return",   early_return_has_loop_and_two_returns);

fn abs_has_one_if(g: &ir::BuiltFunctionGraph) {
    assert!(count_ifs(g) >= 1, "abs_val must have ≥1 If");
}
fn max_has_one_if(g: &ir::BuiltFunctionGraph) {
    assert!(count_ifs(g) >= 1, "max_val must have ≥1 If");
}
fn clamp_has_two_ifs(g: &ir::BuiltFunctionGraph) {
    assert!(count_ifs(g) >= 2, "clamp has 2 conditionals; got {}", count_ifs(g));
}
fn select_three_has_two_ifs(g: &ir::BuiltFunctionGraph) {
    assert!(count_ifs(g) >= 2, "select_three has 2 conditionals; got {}", count_ifs(g));
}
fn sum_to_n_has_loop(g: &ir::BuiltFunctionGraph) {
    assert!(count_loops(g) >= 1, "sum_to_n loop header missing ControlPhi");
}
fn factorial_has_loop(g: &ir::BuiltFunctionGraph) {
    assert!(count_loops(g) >= 1, "factorial loop header missing ControlPhi");
}
fn count_bits_has_loop_and_shr(g: &ir::BuiltFunctionGraph) {
    assert!(count_loops(g) >= 1);
    assert!(count_int_binop(g, ir::IntBinaryOp::ShiftRight) >= 1, "count_bits has x>>=1");
}
fn nested_loops_has_two_loops(g: &ir::BuiltFunctionGraph) {
    assert!(count_loops(g) >= 2, "nested_loops expected ≥2 ControlPhi; got {}", count_loops(g));
}
fn early_return_has_loop_and_two_returns(g: &ir::BuiltFunctionGraph) {
    assert!(count_loops(g) >= 1);
    assert!(count_return_paths(g) >= 2,
            "early_return has 2 source-level return paths; got {}", count_return_paths(g));
}
