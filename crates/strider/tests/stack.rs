//! Stack-frame allocation, StackStoreDetect, and volatile preservation.

#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used, clippy::unreachable)]

mod common;
use common::*;

per_arch_test!("stack", "volatile_three_writes", volatile_preserves_three_stores);
// escape_via_ptr keeps the call alive via an asm-volatile barrier in
// external_take_ptr's body — see fixtures/cases/stack.c.  arm's
// `pop {pc}` placeholder resolves via the indirect-branch resolver's
// `LinkRegister` arm once `StackLoadForward` simplifies the loaded
// target back to `InitialVar(lr)`.
per_arch_test!("stack", "escape_via_ptr", escape_has_stack_store_and_call);
per_arch_test!("stack", "large_local_array",     large_local_has_stack_store_and_loop);
per_arch_test!("stack", "inplace_swap",          swap_has_two_loads_and_two_stores);
// recursive_stack_growth relies on -fno-optimize-sibling-calls in
// fixtures/Makefile to keep the tail call from being elided.
per_arch_test!("stack", "recursive_stack_growth", rec_stack_has_call_and_stores);

fn volatile_preserves_three_stores(g: &strider_ir::BuiltFunctionGraph) {
    // *p = v; *p = v+1; *p = v+2  — opt must not collapse these.
    assert!(count_stores(g) >= 3,
            "volatile must preserve 3 stores; got {}", count_stores(g));
}
fn escape_has_stack_store_and_call(g: &strider_ir::BuiltFunctionGraph) {
    assert!(count_stores(g) >= 1, "&local forces a stack write");
    assert!(count_calls(g) >= 1, "external_take_ptr is called");
}
fn large_local_has_stack_store_and_loop(g: &strider_ir::BuiltFunctionGraph) {
    assert!(count_stores(g) >= 1, "buf[i] = i*i is a stack store");
    assert!(count_loops(g) >= 1);
}
fn swap_has_two_loads_and_two_stores(g: &strider_ir::BuiltFunctionGraph) {
    assert!(count_loads(g) >= 2, "expected ≥2 Loads; got {}", count_loads(g));
    assert!(count_stores(g) >= 2, "expected ≥2 Stores; got {}", count_stores(g));
}
fn rec_stack_has_call_and_stores(g: &strider_ir::BuiltFunctionGraph) {
    assert!(count_calls(g) >= 1, "self-recursive call");
    assert!(count_stores(g) >= 1, "buf[i] writes");
}
