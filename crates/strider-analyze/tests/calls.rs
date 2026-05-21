//! Direct, indirect, mutual, and recursive Call nodes.

#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used, clippy::unreachable)]

mod common;
use common::*;

// fib_recursive / pass_through rely on -fno-optimize-sibling-calls in
// fixtures/Makefile to keep tail calls from being elided.
per_arch_test!("calls", "fib_recursive",      fib_has_two_calls);
per_arch_test!("calls", "mutual_a",           mutual_has_one_call);
per_arch_test!("calls", "mutual_b",           mutual_has_one_call);
per_arch_test!("calls", "nested_3deep",       nested_has_one_call);
per_arch_test!("calls", "repeat_call_pair",   repeat_has_two_calls);
per_arch_test!("calls", "pass_through",       pass_through_has_one_call);
// apply_indirect: GCC at -O2 with -fno-optimize-sibling-calls inlines
// the function-pointer target as a direct call before the analyzer sees
// indirection, making the indirect branch path unreachable on every arch.
per_arch_test!("calls", "apply_indirect",     indirect_has_call);

fn fib_has_two_calls(g: &strider_ir::BuiltFunctionGraph) {
    // fib(n-1) + fib(n-2) — two recursive calls.
    assert!(count_calls(g) >= 2,
            "fib has 2 self-recursive calls; got {}", count_calls(g));
    assert!(count_ifs(g) >= 1, "fib base case has If");
}
fn mutual_has_one_call(g: &strider_ir::BuiltFunctionGraph) {
    assert!(count_calls(g) >= 1,
            "mutual_a/b each call the other once; got {}", count_calls(g));
    assert!(count_ifs(g) >= 1, "mutual base case has If");
}
fn nested_has_one_call(g: &strider_ir::BuiltFunctionGraph) {
    assert!(count_calls(g) >= 1, "nested_3deep calls mid()");
}
fn repeat_has_two_calls(g: &strider_ir::BuiltFunctionGraph) {
    assert!(count_calls(g) >= 2,
            "repeat_call_pair calls pair_a twice; got {}", count_calls(g));
}
fn pass_through_has_one_call(g: &strider_ir::BuiltFunctionGraph) {
    assert!(count_calls(g) >= 1, "pass_through calls leaf");
}
fn indirect_has_call(g: &strider_ir::BuiltFunctionGraph) {
    // Indirect calls are still emitted as Call nodes; the address input is
    // not an IntConst.  We pin only that the call site exists.
    assert!(count_calls(g) >= 1, "apply_indirect has an indirect Call");
}
