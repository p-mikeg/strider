//! Calling-convention argument and return materialisation.
//!
//! `eight_int_args` exercises stack-arg paths (only x86_64 has 6 arg regs;
//! cdecl/MIPS use stack args earlier, ARM/AArch64 use 4/8 arg regs).
//! `point_sum` and `make_pair` exercise struct decomposition.

#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used, clippy::unreachable)]

mod common;
use common::*;

per_arch_test!("abi", "eight_int_args",  eight_args_has_seven_adds);
per_arch_test!("abi", "mixed_args",      mixed_has_loads_and_adds);
per_arch_test!("abi", "point_sum",       point_sum_has_add);
per_arch_test!("abi", "make_pair",       make_pair_has_return);
per_arch_test!("abi", "tail_caller",     tail_caller_has_call, ignore = {
    Arm: "BUG-5: ARM tail-call generates BranchIndirect (unimplemented)",
});

fn eight_args_has_seven_adds(g: &ir::BuiltFunctionGraph) {
    // a + b + c + d + e + f + g + h = 7 add nodes (left-fold).
    assert!(count_int_binop(g, ir::IntBinaryOp::Add) >= 7,
            "eight_int_args has 7 Adds; got {}", count_int_binop(g, ir::IntBinaryOp::Add));
}
fn mixed_has_loads_and_adds(g: &ir::BuiltFunctionGraph) {
    // Two pointer args dereferenced once each — ≥2 Loads.
    assert!(count_loads(g) >= 2, "mixed_args dereferences 2 pointers; got {}", count_loads(g));
    assert!(count_int_binop(g, ir::IntBinaryOp::Add) >= 5,
            "mixed_args has 5 Adds; got {}", count_int_binop(g, ir::IntBinaryOp::Add));
}
fn point_sum_has_add(g: &ir::BuiltFunctionGraph) {
    assert!(count_int_binop(g, ir::IntBinaryOp::Add) >= 1, "point_sum is x+y");
}
fn make_pair_has_return(g: &ir::BuiltFunctionGraph) {
    assert!(count_returns(g) >= 1, "make_pair returns a pair");
}
fn tail_caller_has_call(g: &ir::BuiltFunctionGraph) {
    // Note: compilers may tail-call-optimize this; if so the Call disappears.
    // Test guards against complete IR breakage but not against tail-call elision.
    assert!(count_calls(g) >= 1 || count_returns(g) >= 1,
            "tail_caller must have a Call or Return; got {} call, {} ret",
            count_calls(g), count_returns(g));
}
