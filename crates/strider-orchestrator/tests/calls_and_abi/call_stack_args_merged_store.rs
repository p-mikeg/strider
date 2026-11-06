//! gcc's cross-jumping merges two call sequences that differ in one stack
//! argument: each arm stores its own value, then jumps into the shared
//! stores and call. The argument is then the phi of the arms' values.
//! i386 cdecl: arg 0 is `[esp]` at the call.

use crate::common;
use crate::common::Arch;
use strider_ir::node::{NodeKind, ValueId};
use strider_ir::{IRViewer, IRWalker};

const BASE: u64 = 0x1000;
/// `[control, memory, target, sp]`: cdecl passes no argument in a register.
const CDECL_ARITY: usize = 4;

/// The stack arguments collected onto the one call in `bytes`.
fn stack_args(bytes: Vec<u8>) -> (strider_ir::Function, Vec<ValueId>) {
    let (mut strider, cc) = common::strider_over_bytes(Arch::X86, bytes, BASE, None);
    let function = strider
        .analyze(BASE, &cc, &Default::default(), None)
        .expect("analyze")
        .function;
    let call = function
        .walk()
        .find(|&n| matches!(function.node_kind(n), NodeKind::Call { .. }))
        .expect("one Call");
    let args = function
        .node_inputs(call)
        .into_iter()
        .skip(CDECL_ARITY)
        .collect();
    (function, args)
}

#[test]
fn a_slot_stored_on_both_merging_arms_is_a_phi_argument() {
    // test ecx,ecx; je 1f; push 10; jmp 2f; 1: push 11; 2: push 9;
    // call ext; add esp,8; ret; ext: ret
    let (function, args) = stack_args(vec![
        0x85, 0xc9, 0x74, 0x04, 0x6a, 0x0a, 0xeb, 0x02, 0x6a, 0x0b, 0x6a, 0x09, 0xe8, 0x04, 0x00,
        0x00, 0x00, 0x83, 0xc4, 0x08, 0xc3, 0xc3,
    ]);
    assert_eq!(args.len(), 2, "both stack arguments are collected");
    assert_eq!(function.int_const_u128(args[0]), Some(9));
    let phi = function.producer(args[1]);
    assert!(matches!(function.node_kind(phi), NodeKind::Phi));
    let mut arms: Vec<_> = function
        .phi_data_inputs(phi)
        .map(|v| function.int_const_u128(v))
        .collect();
    arms.sort_unstable();
    assert_eq!(arms, [Some(10), Some(11)]);
}

#[test]
fn a_slot_both_arms_store_alike_is_that_value() {
    // As above, both arms `push 10`.
    let (function, args) = stack_args(vec![
        0x85, 0xc9, 0x74, 0x04, 0x6a, 0x0a, 0xeb, 0x02, 0x6a, 0x0a, 0x6a, 0x09, 0xe8, 0x04, 0x00,
        0x00, 0x00, 0x83, 0xc4, 0x08, 0xc3, 0xc3,
    ]);
    let values: Vec<_> = args.iter().map(|&v| function.int_const_u128(v)).collect();
    assert_eq!(values, [Some(9), Some(10)]);
}

/// At a loop header the arms are one local's stores across iterations.
#[test]
fn a_slot_stored_before_and_around_a_loop_is_no_argument() {
    // sub esp,12; mov dword [esp+4],5; 1: mov dword [esp],1; call ext;
    // mov dword [esp+4],6; dec ecx; jne 1b; add esp,12; ret; ext: ret
    let (function, args) = stack_args(vec![
        0x83, 0xec, 0x0c, 0xc7, 0x44, 0x24, 0x04, 0x05, 0x00, 0x00, 0x00, 0xc7, 0x04, 0x24, 0x01,
        0x00, 0x00, 0x00, 0xe8, 0x0f, 0x00, 0x00, 0x00, 0xc7, 0x44, 0x24, 0x04, 0x06, 0x00, 0x00,
        0x00, 0x49, 0x75, 0xe9, 0x83, 0xc4, 0x0c, 0xc3, 0xc3,
    ]);
    let values: Vec<_> = args.iter().map(|&v| function.int_const_u128(v)).collect();
    assert_eq!(values, [Some(1)]);
}
