//! Calling-convention argument and return materialisation.
//!
//! `eight_int_args` exercises stack-arg paths (only x86_64 has 6 arg regs;
//! cdecl/MIPS use stack args earlier, ARM/AArch64 use 4/8 arg regs).
//! `point_sum` and `make_pair` exercise struct decomposition.

#![allow(
    clippy::panic,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::unreachable
)]

mod common;
use common::*;

per_arch_test!("abi", "eight_int_args", eight_args_has_seven_adds);
per_arch_test!("abi", "mixed_args", mixed_has_loads_and_adds);
per_arch_test!("abi", "point_sum", point_sum_has_add);
per_arch_test!("abi", "make_pair", make_pair_has_return);
// tail_caller: closed under the indirect-branch fixed-point
// design (per `2026-04-27-indirect-branch-fixedpoint.md`).
// the IR-level orchestrator resolver's `LinkRegister` arm classifies arm `pop {pc}` (load + bx)
// once `LoadForward` simplifies the loaded target back to
// `InitialVar(lr)`, so the placeholder Return resolves to a real
// Return at the cfg-rebuild step.  All arches, including arm, now
// pass without an ignore.
per_arch_test!("abi", "tail_caller", tail_caller_has_call);

fn eight_args_has_seven_adds(function: &strider_ir::Function) {
    // a + b + c + d + e + f + g + h = 7 add nodes (left-fold).
    assert!(
        count_int_binop(function, strider_ir::IntBinaryOp::Add) >= 7,
        "eight_int_args has 7 Adds; got {}",
        count_int_binop(function, strider_ir::IntBinaryOp::Add)
    );
}
fn mixed_has_loads_and_adds(function: &strider_ir::Function) {
    // Two pointer args dereferenced once each — ≥2 Loads.
    assert!(
        count_loads(function) >= 2,
        "mixed_args dereferences 2 pointers; got {}",
        count_loads(function)
    );
    assert!(
        count_int_binop(function, strider_ir::IntBinaryOp::Add) >= 5,
        "mixed_args has 5 Adds; got {}",
        count_int_binop(function, strider_ir::IntBinaryOp::Add)
    );
}
fn point_sum_has_add(function: &strider_ir::Function) {
    assert!(
        count_int_binop(function, strider_ir::IntBinaryOp::Add) >= 1,
        "point_sum is x+y"
    );
}
fn make_pair_has_return(function: &strider_ir::Function) {
    assert!(count_returns(function) >= 1, "make_pair returns a pair");
}
fn tail_caller_has_call(function: &strider_ir::Function) {
    // Note: compilers may tail-call-optimize this; if so the Call disappears.
    // Test guards against complete IR breakage but not against tail-call elision.
    assert!(
        count_calls(function) >= 1 || count_returns(function) >= 1,
        "tail_caller must have a Call or Return; got {} call, {} ret",
        count_calls(function),
        count_returns(function)
    );
}
