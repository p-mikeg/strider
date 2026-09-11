//! `CallStackArgCollect` wires positional stack-passed arguments into `Call`
//! nodes as extra value inputs past the base `[Control, Memory, Target, SP]`.
//!
//! x86 only: cdecl's `arg_passing_regs` is empty, so every argument of
//! `calling_convention.c::forward_8`'s `sink8(a..h)` call is stack-pushed.
//! Register-rich arches pass all eight in registers, leaving this pass nothing
//! to collect.

mod common;
use common::*;

use strider_ir::node::{NodeId, NodeKind};
use strider_ir::{IRViewer, IRWalker};

/// Base input count of a `Call` node: Control, Memory, Target, SP.  Any input
/// past these four is a collected positional argument.
const CALL_BASE_INPUTS: usize = 4;

#[test]
fn forward_8_call_has_stack_args_collected_x86() {
    let function = analyze(Arch::X86, "calling_convention", "forward_8");

    let calls: Vec<NodeId> = function
        .walk()
        .filter(|&n| matches!(function.node_kind(n), NodeKind::Call))
        .collect();
    assert!(
        !calls.is_empty(),
        "forward_8 must lift to >= 1 Call (the sink8 call site)"
    );

    // At least one Call must have more than the 4 base inputs: only
    // CallStackArgCollect appends Call inputs, so this pins the pass.
    let max_inputs = calls
        .iter()
        .map(|&c| function.node_inputs(c).len())
        .max()
        .unwrap();
    assert!(
        max_inputs > CALL_BASE_INPUTS,
        "expected a Call on x86 cdecl to have > {CALL_BASE_INPUTS} inputs \
         (base ctrl/mem/target/sp + collected stack args); the widest Call had \
         {max_inputs} inputs, so CallStackArgCollect wired no stack args"
    );
}
