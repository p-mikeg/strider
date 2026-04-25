//! Branches, loops, and merge points.
//!
//! 9 functions × 6 archs = 54 tests.

#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used, clippy::unreachable)]

mod common;
use common::*;

per_arch_test!("control", "abs_val",        abs_has_one_if);
per_arch_test!("control", "max_val",        max_has_one_if, ignore = {
    Mips32le: "BUG-2: CFG builder rejects MIPS comparison's CONST-space branch target",
    Mips32be: "BUG-2: CFG builder rejects MIPS comparison's CONST-space branch target",
});
per_arch_test!("control", "clamp",          clamp_has_two_ifs, ignore = {
    Mips32le: "BUG-3: MIPS comparison emits Bool where downstream node expects AnyInt (IR validator)",
    Mips32be: "BUG-3: MIPS comparison emits Bool where downstream node expects AnyInt (IR validator)",
});
per_arch_test!("control", "select_three",   select_three_has_two_ifs, ignore = {
    Arm: "BUG-4: ARM conditional select emits non-Bool to a node expecting Bool",
});
per_arch_test!("control", "sum_to_n",       sum_to_n_has_loop);
per_arch_test!("control", "factorial",      factorial_has_loop, ignore = {
    Mips32le: "BUG-3: MIPS comparison emits Bool where downstream node expects AnyInt",
    Mips32be: "BUG-3: MIPS comparison emits Bool where downstream node expects AnyInt",
});
per_arch_test!("control", "count_bits",     count_bits_has_loop_and_shr);
per_arch_test!("control", "nested_loops",   nested_loops_has_two_loops, ignore = {
    Arm: "BUG-5: BranchIndirect p-code opcode unimplemented (ARM jump table)",
});
per_arch_test!("control", "early_return",   early_return_has_loop_and_two_returns, ignore = {
    Arm: "BUG-3: ARM comparison emits Bool where downstream node expects AnyInt",
});

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
    assert!(count_returns(g) >= 2, "early_return has 2 return paths; got {}", count_returns(g));
}
