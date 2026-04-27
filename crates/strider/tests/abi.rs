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
// tail_caller: pre-Phase-5 BUG-5 fix collapsed BranchIndirect into
// Return.  Phase 5 replaces the blanket mapping with a real resolver
// that proves the target via ConstantFold + KnownBits + RedundantPhis
// + (optional) LoadReadOnly.  ARM `pop {pc}` (single-register pop on
// gcc -O1+) lifts to `load tmp = [sp]; sp = sp + 4; BranchIndirect tmp`.
// The resolver does not (yet) model stack-store-forward (StackStoreDetect
// + StackLoadForward), so it cannot prove `tmp` is the entry value of
// `lr`.  Consequence: the arm `tail_caller` fixture trips
// `UnresolvedIndirectBranch`.  Tracked under BUG-5 — extending the
// resolver with stack-load forwarding is the canonical fix and is
// future work documented in the indirect-branch resolution spec.
// Multi-register `pop {fp, pc}` emits `Return` directly in the SLA
// spec, so the other ARM/AArch64/PowerPC/MIPS arches sidestep this.
per_arch_test!(
    "abi", "tail_caller", tail_caller_has_call,
    ignore = {
        Arm: "BUG-5 residue: arm `pop {pc}` lifts to load+BranchIndirect; resolver lacks stack-load-forward",
    }
);

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
