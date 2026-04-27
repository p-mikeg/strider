//! Stack-frame allocation, StackStoreDetect, and volatile preservation.

#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used, clippy::unreachable)]

mod common;
use common::*;

per_arch_test!("stack", "volatile_three_writes", volatile_preserves_three_stores);
// escape_via_ptr: BUG-12 (call elided) is fixed by an asm-volatile barrier
// in external_take_ptr's body — see fixtures/cases/stack.c.
// Phase 5's resolver replaces the legacy BranchIndirect→Return mapping;
// arm `pop {pc}` (load + BranchIndirect) is unresolvable without stack-
// load forwarding.  Tracked under BUG-5.
per_arch_test!(
    "stack", "escape_via_ptr", escape_has_stack_store_and_call,
    ignore = {
        Arm: "BUG-5 residue: arm `pop {pc}` lifts to load+BranchIndirect; resolver lacks stack-load-forward",
    }
);
// large_local_array: BUG-13 (AArch64 U128 array-init constant) is fixed by
// the IntConst u128 widening.  BUG-14 (ARM optimizer panic) is fixed
// transitively by BUG-3's coerce-on-write at write_reg_vn (the panic was
// rooted in the same Bool→AnyInt edge after RedundantPhis).
per_arch_test!("stack", "large_local_array",     large_local_has_stack_store_and_loop);
per_arch_test!("stack", "inplace_swap",          swap_has_two_loads_and_two_stores);
// recursive_stack_growth: BUG-6 (tail-call elision) fixed by Makefile flag.
per_arch_test!("stack", "recursive_stack_growth", rec_stack_has_call_and_stores);

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
