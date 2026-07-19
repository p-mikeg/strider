//! End-to-end validity test for the `CallStackArgCollect` optimizer post-pass.
//!
//! `CallStackArgCollect` wires positional stack-passed arguments into `Call`
//! nodes as extra value inputs. A `Call`'s base input shape is `[Control,
//! Memory, Target, SP]` (4 inputs, see `node_signature` for `NodeKind::Call`);
//! every input beyond those four is a collected argument.
//!
//! Single-arch (x86) test: on x86 cdecl `arg_passing_regs` is empty (all
//! arguments stack-passed), so a `Call` with more than 4 inputs proves
//! `CallStackArgCollect` appended them (no other pass adds Call inputs).
//! Register-rich arches pass low-arity args in registers, leaving nothing on
//! the stack for this pass to collect.
//!
//! Fixture: `calling_convention.c::forward_8` makes one call,
//! `sink8(a, b, c, d, e, f, g, h)`, whose 8 int arguments are all
//! stack-pushed on x86 cdecl.

#![allow(
    clippy::panic,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::unreachable
)]

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
         {max_inputs} inputs — CallStackArgCollect did not wire any stack args"
    );
}
