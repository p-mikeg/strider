//! Direct, indirect, mutual, and recursive Call nodes.

#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used, clippy::unreachable)]

mod common;
use common::*;

per_arch_test!("calls", "fib_recursive",      fib_has_two_calls, ignore = {
    X86:      "BUG-6: compiler tail-call elision drops second recursive call",
    X64:      "BUG-6: compiler tail-call elision drops second recursive call",
    Aarch64:  "BUG-6: compiler tail-call elision drops second recursive call",
    Arm:      "BUG-6: compiler tail-call elision drops second recursive call",
    Mips32le: "BUG-6: compiler tail-call elision drops second recursive call",
    Mips32be: "BUG-6: compiler tail-call elision drops second recursive call",
});
per_arch_test!("calls", "mutual_a",           mutual_has_one_call);
per_arch_test!("calls", "mutual_b",           mutual_has_one_call);
per_arch_test!("calls", "nested_3deep",       nested_has_one_call);
per_arch_test!("calls", "repeat_call_pair",   repeat_has_two_calls);
per_arch_test!("calls", "pass_through",       pass_through_has_one_call, ignore = {
    X86:      "BUG-6: compiler tail-call elision turns simple wrapper into a jump",
    X64:      "BUG-6: compiler tail-call elision turns simple wrapper into a jump",
    Aarch64:  "BUG-6: compiler tail-call elision turns simple wrapper into a jump",
    Arm:      "BUG-6: compiler tail-call elision turns simple wrapper into a jump",
    Mips32le: "BUG-6: compiler tail-call elision turns simple wrapper into a jump",
    Mips32be: "BUG-6: compiler tail-call elision turns simple wrapper into a jump",
});
per_arch_test!("calls", "apply_indirect",     indirect_has_call, ignore = {
    X86:      "BUG-7: indirect call follows fn-pointer into unmapped memory (CFG MemReadErr)",
    X64:      "BUG-7: indirect call follows fn-pointer into unmapped memory (CFG MemReadErr)",
    Aarch64:  "BUG-5: BranchIndirect p-code opcode unimplemented",
    Arm:      "BUG-5: BranchIndirect p-code opcode unimplemented",
    Mips32le: "BUG-5: BranchIndirect p-code opcode unimplemented",
    Mips32be: "BUG-5: BranchIndirect p-code opcode unimplemented",
});

fn fib_has_two_calls(g: &ir::BuiltFunctionGraph) {
    // fib(n-1) + fib(n-2) — two recursive calls.
    assert!(count_calls(g) >= 2,
            "fib has 2 self-recursive calls; got {}", count_calls(g));
    assert!(count_ifs(g) >= 1, "fib base case has If");
}
fn mutual_has_one_call(g: &ir::BuiltFunctionGraph) {
    assert!(count_calls(g) >= 1,
            "mutual_a/b each call the other once; got {}", count_calls(g));
    assert!(count_ifs(g) >= 1, "mutual base case has If");
}
fn nested_has_one_call(g: &ir::BuiltFunctionGraph) {
    assert!(count_calls(g) >= 1, "nested_3deep calls mid()");
}
fn repeat_has_two_calls(g: &ir::BuiltFunctionGraph) {
    assert!(count_calls(g) >= 2,
            "repeat_call_pair calls pair_a twice; got {}", count_calls(g));
}
fn pass_through_has_one_call(g: &ir::BuiltFunctionGraph) {
    assert!(count_calls(g) >= 1, "pass_through calls leaf");
}
fn indirect_has_call(g: &ir::BuiltFunctionGraph) {
    // Indirect calls are still emitted as Call nodes; the address input is
    // not an IntConst.  We pin only that the call site exists.
    assert!(count_calls(g) >= 1, "apply_indirect has an indirect Call");
}
