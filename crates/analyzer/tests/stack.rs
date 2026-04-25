//! Stack-frame allocation, StackStoreDetect, and volatile preservation.

#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used, clippy::unreachable)]

mod common;
use common::*;

per_arch_test!("stack", "volatile_three_writes", volatile_preserves_three_stores);
per_arch_test!("stack", "escape_via_ptr",        escape_has_stack_store_and_call, ignore = {
    X86:      "BUG-12: external_take_ptr call not emitted as Call node",
    X64:      "BUG-12: external_take_ptr call not emitted as Call node",
    Aarch64:  "BUG-12: external_take_ptr call not emitted as Call node",
    Arm:      "BUG-12: external_take_ptr call not emitted as Call node",
    Mips32le: "BUG-12: external_take_ptr call not emitted as Call node",
    Mips32be: "BUG-12: external_take_ptr call not emitted as Call node",
});
per_arch_test!("stack", "large_local_array",     large_local_has_stack_store_and_loop, ignore = {
    Aarch64: "BUG-13: AArch64 emits 128-bit constant for array init; analyzer can't store u128",
    Arm:     "BUG-14: optimizer pipeline panics on ARM large_local_array",
});
per_arch_test!("stack", "inplace_swap",          swap_has_two_loads_and_two_stores);
per_arch_test!("stack", "recursive_stack_growth", rec_stack_has_call_and_stores, ignore = {
    X86:      "BUG-6: compiler tail-call elision converts recursive call into branch",
    X64:      "BUG-6: compiler tail-call elision converts recursive call into branch",
    Aarch64:  "BUG-6: compiler tail-call elision converts recursive call into branch",
    Arm:      "BUG-6: compiler tail-call elision converts recursive call into branch",
    Mips32le: "BUG-6: compiler tail-call elision converts recursive call into branch",
    Mips32be: "BUG-6: compiler tail-call elision converts recursive call into branch",
});

fn volatile_preserves_three_stores(g: &ir::BuiltFunctionGraph) {
    // *p = v; *p = v+1; *p = v+2  — opt must not collapse these.
    assert!(count_stores(g) >= 3,
            "volatile must preserve 3 stores; got {}", count_stores(g));
}
fn escape_has_stack_store_and_call(g: &ir::BuiltFunctionGraph) {
    assert!(count_stores(g) >= 1, "&local forces a stack write");
    assert!(count_calls(g) >= 1, "external_take_ptr is called");
}
fn large_local_has_stack_store_and_loop(g: &ir::BuiltFunctionGraph) {
    assert!(count_stores(g) >= 1, "buf[i] = i*i is a stack store");
    assert!(count_loops(g) >= 1);
}
fn swap_has_two_loads_and_two_stores(g: &ir::BuiltFunctionGraph) {
    assert!(count_loads(g) >= 2, "expected ≥2 Loads; got {}", count_loads(g));
    assert!(count_stores(g) >= 2, "expected ≥2 Stores; got {}", count_stores(g));
}
fn rec_stack_has_call_and_stores(g: &ir::BuiltFunctionGraph) {
    assert!(count_calls(g) >= 1, "self-recursive call");
    assert!(count_stores(g) >= 1, "buf[i] writes");
}
