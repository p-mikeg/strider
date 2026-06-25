//! End-to-end validity test for the `CallStackArgCollect` optimizer post-pass.
//!
//! `CallStackArgCollect` wires positional stack-passed arguments into `Call`
//! nodes as extra value inputs.  A `Call`'s base input shape is
//! `[Control, Memory, Target, SP]` (4 inputs — see `node_signature` for
//! `NodeKind::Call`); every input BEYOND those four is a collected argument.
//!
//! On **x86 cdecl** `arg_passing_regs` is empty (all arguments are stack-
//! passed), so the only way a `Call` ends up with more than 4 inputs is if
//! `CallStackArgCollect` appended the stack args.  No other pass adds Call
//! inputs.  This is therefore a single-arch (x86) test: register-rich arches
//! pass low-arity args in registers, leaving nothing on the stack for this
//! pass to collect.
//!
//! Fixture: `calling_convention.c::forward_8`, whose body makes exactly one
//! call — `sink8(a, b, c, d, e, f, g, h)` with 8 int arguments.  On x86 cdecl
//! all 8 are pushed to the stack, so the lifted `Call` to sink8 must gain
//! stack-arg inputs.  The assertion fails if `CallStackArgCollect` is removed.

#![allow(
    clippy::panic,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::unreachable
)]

mod common;
use common::*;

use strider_ir::{
    IRViewer, IRWalker,
    node::{NodeId, NodeKind},
};

/// Base input count of a `Call` node: Control, Memory, Target, SP.  Any input
/// past these four is a collected positional argument.
const CALL_BASE_INPUTS: usize = 4;

#[test]
fn forward_8_call_has_stack_args_collected_x86() {
    // x86 cdecl: every argument is stack-passed (arg_passing_regs == []).
    let function = analyze(Arch::X86, "calling_convention", "forward_8");

    let calls: Vec<NodeId> = function
        .walk()
        .filter(|&n| matches!(function.node_kind(n), NodeKind::Call))
        .collect();
    assert!(
        !calls.is_empty(),
        "forward_8 must lift to >= 1 Call (the sink8 call site)"
    );

    // At least one Call must have MORE than the 4 base inputs — i.e. it gained
    // stack-arg inputs.  Only CallStackArgCollect appends Call inputs, so this
    // pins the pass: it would fail if CallStackArgCollect were removed.
    let max_inputs = calls
        .iter()
        .map(|&c| function.node_inputs(c).len())
        .max()
        .unwrap();
    assert!(
        max_inputs > CALL_BASE_INPUTS,
        "expected a Call on x86 cdecl to have > {CALL_BASE_INPUTS} inputs \
         (base ctrl/mem/target/sp + collected stack args); the widest Call had \
         {max_inputs} inputs — CallStackArgCollect did not wire any stack args"
    );
}
