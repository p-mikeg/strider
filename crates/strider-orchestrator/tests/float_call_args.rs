//! Float / vector arguments reach a `Call` as inputs.
//!
//! A `Call`'s inputs cover both `cc.arg_passing_regs` (integer) and the float
//! argument registers.  A float argument left out of them has no consumer: the
//! call's clobber output overwrites the register and DCE deletes the whole
//! argument cone.
//!
//! Float arguments are APPENDED to the integer ones, so `call().arg(N)` keeps
//! its meaning for every existing integer query and the first float argument
//! sits at `arg(cc.arg_passing_regs.len())`, index 6 on x86-64 SysV.

mod common;
use common::*;

use strider_ir::node::{NodeId, NodeKind};
use strider_ir::{IRViewer, IRWalker};
use strider_pattern::{Capture, CaptureExt, Matcher, anything, call};

/// `[Control, Memory, Target, SP]` precede the argument list.
const CALL_BASE_INPUTS: usize = 4;
/// x86-64 SysV integer argument registers: RDI, RSI, RDX, RCX, R8, R9.
const X64_INT_ARGS: usize = 6;

fn calls(function: &strider_ir::Function) -> Vec<NodeId> {
    function
        .walk()
        .filter(|&n| matches!(function.node_kind(n), NodeKind::Call))
        .collect()
}

/// `floats.c::main` calls `f32_arith` / `f64_arith` / ... with their arguments
/// in XMM0 / XMM1, so every one of its call sites carries float arguments past
/// the six integer ones.
#[test]
fn x64_call_carries_xmm_arguments_after_the_integer_ones() {
    let function = analyze(Arch::X64, "floats", "main");
    let calls = calls(&function);
    assert!(!calls.is_empty(), "floats::main must lift to >= 1 Call");

    for call_id in calls {
        let inputs = function.node_inputs(call_id);
        assert!(
            inputs.len() > CALL_BASE_INPUTS + X64_INT_ARGS,
            "Call {call_id:?} has {} inputs; expected more than the \
             {CALL_BASE_INPUTS} structural ones plus {X64_INT_ARGS} integer \
             arguments, i.e. at least one XMM argument",
            inputs.len(),
        );
        // The integer argument registers are 8 bytes wide and keep slots
        // 4..10; a float argument register is a 16-byte XMM container.
        for slot in CALL_BASE_INPUTS..CALL_BASE_INPUTS + X64_INT_ARGS {
            assert_eq!(
                function.value_type(inputs[slot]).unwrap().byte_size(),
                8,
                "Call {call_id:?} input slot {slot} must still hold an 8-byte \
                 integer argument register",
            );
        }
        for slot in CALL_BASE_INPUTS + X64_INT_ARGS..inputs.len() {
            assert_eq!(
                function.value_type(inputs[slot]).unwrap().byte_size(),
                16,
                "Call {call_id:?} input slot {slot} must hold a 16-byte XMM \
                 argument register",
            );
        }
    }
}

/// The pattern DSL must reach a float argument: on x86-64 SysV the first one
/// is `arg(6)`, directly after the six integer argument registers.
#[test]
fn x64_pattern_query_reaches_the_first_float_argument() {
    let function = analyze(Arch::X64, "floats", "main");
    let m = Matcher::new(&function);

    let arg = Capture::new();
    let pat = call().arg(X64_INT_ARGS, anything().capture(arg)).build();
    let hits = m.find_all(&pat).unwrap();
    assert!(
        !hits.is_empty(),
        "call().arg({X64_INT_ARGS}, ..) must bind the first float argument",
    );
    for hit in &hits {
        let value = hit.value(arg).expect("arg capture is bound");
        assert_eq!(
            function.value_type(value).unwrap().byte_size(),
            16,
            "arg({X64_INT_ARGS}) must be the 16-byte XMM0 argument",
        );
    }
}

/// Integer argument positions are pinned: an integer-only call site on x86-64
/// still binds its six integer registers at `arg(0)`..`arg(5)`, each an 8-byte
/// value, with nothing interleaved.
#[test]
fn x64_integer_argument_positions_are_unchanged() {
    let function = analyze(Arch::X64, "abi", "tail_caller");
    let m = Matcher::new(&function);

    for i in 0..X64_INT_ARGS {
        let arg = Capture::new();
        let pat = call().arg(i, anything().capture(arg)).build();
        let hits = m.find_all(&pat).unwrap();
        assert!(!hits.is_empty(), "call().arg({i}, ..) must bind");
        for hit in &hits {
            let value = hit.value(arg).expect("arg capture is bound");
            assert_eq!(
                function.value_type(value).unwrap().byte_size(),
                8,
                "arg({i}) must still be an 8-byte integer argument register",
            );
        }
    }
}
