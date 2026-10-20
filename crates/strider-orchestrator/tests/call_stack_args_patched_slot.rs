//! A stack argument some path partly overwrites before the call holds no one
//! value, so `CallStackArgCollect` must not attach the original store's data.
//! i386 cdecl: arg 0 is `[esp]` at the call.

mod common;
use common::Arch;
use strider_ir::node::{NodeKind, ValueId};
use strider_ir::{IRViewer, IRWalker};

const BASE: u64 = 0x1000;
/// `[control, memory, target, sp]`: cdecl passes no argument in a register.
const CDECL_ARITY: usize = 4;

/// The stack arguments collected onto the one call in `bytes`.
fn stack_args(bytes: Vec<u8>) -> (strider_ir::Function, Vec<ValueId>) {
    let (mut strider, cc) = common::strider_over_bytes(Arch::X86, bytes, BASE, None);
    let result = strider
        .analyze(BASE, &cc, &Default::default(), None)
        .expect("analyze");
    let function = result.function;
    assert_eq!(common::count_calls(&function), 1);
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
fn one_arm_patching_byte_1_leaves_no_value() {
    // sub esp,12; mov dword [esp],0x11223344; test ecx,ecx; je 1f;
    // mov byte [esp+1],0x55; 1: call ext; add esp,12; ret; ext: ret
    let (_, args) = stack_args(vec![
        0x83, 0xec, 0x0c, 0xc7, 0x04, 0x24, 0x44, 0x33, 0x22, 0x11, 0x85, 0xc9, 0x74, 0x05, 0xc6,
        0x44, 0x24, 0x01, 0x55, 0xe8, 0x04, 0x00, 0x00, 0x00, 0x83, 0xc4, 0x0c, 0xc3, 0xc3,
    ]);
    assert_eq!(args, []);
}

#[test]
fn both_arms_patching_byte_2_differently_leave_no_value() {
    // sub esp,12; mov dword [esp],0x11223344; test ecx,ecx; je 1f;
    // mov byte [esp+2],0x55; jmp 2f; 1: mov byte [esp+2],0x66;
    // 2: call ext; add esp,12; ret; ext: ret
    let (_, args) = stack_args(vec![
        0x83, 0xec, 0x0c, 0xc7, 0x04, 0x24, 0x44, 0x33, 0x22, 0x11, 0x85, 0xc9, 0x74, 0x07, 0xc6,
        0x44, 0x24, 0x02, 0x55, 0xeb, 0x05, 0xc6, 0x44, 0x24, 0x02, 0x66, 0xe8, 0x04, 0x00, 0x00,
        0x00, 0x83, 0xc4, 0x0c, 0xc3, 0xc3,
    ]);
    assert_eq!(args, []);
}

#[test]
fn double_with_upper_dword_zeroed_on_one_path_leaves_no_value() {
    // sub esp,12; fld qword [ecx]; fstp qword [esp]; test edx,edx; je 1f;
    // mov dword [esp+4],0; 1: call ext; add esp,12; ret; ext: ret
    let (_, args) = stack_args(vec![
        0x83, 0xec, 0x0c, 0xdd, 0x01, 0xdd, 0x1c, 0x24, 0x85, 0xd2, 0x74, 0x08, 0xc7, 0x44, 0x24,
        0x04, 0x00, 0x00, 0x00, 0x00, 0xe8, 0x04, 0x00, 0x00, 0x00, 0x83, 0xc4, 0x0c, 0xc3, 0xc3,
    ]);
    assert_eq!(args, []);
}

#[test]
fn straight_line_patch_leaves_no_value() {
    // sub esp,12; mov dword [esp],0x11223344; mov byte [esp+1],0x55;
    // call ext; add esp,12; ret; ext: ret
    let (_, args) = stack_args(vec![
        0x83, 0xec, 0x0c, 0xc7, 0x04, 0x24, 0x44, 0x33, 0x22, 0x11, 0xc6, 0x44, 0x24, 0x01, 0x55,
        0xe8, 0x04, 0x00, 0x00, 0x00, 0x83, 0xc4, 0x0c, 0xc3, 0xc3,
    ]);
    assert_eq!(args, []);
}

#[test]
fn one_arm_patching_byte_0_leaves_no_value() {
    // sub esp,12; mov dword [esp],0x11223344; test ecx,ecx; je 1f;
    // mov byte [esp],0x55; 1: call ext; add esp,12; ret; ext: ret
    let (_, args) = stack_args(vec![
        0x83, 0xec, 0x0c, 0xc7, 0x04, 0x24, 0x44, 0x33, 0x22, 0x11, 0x85, 0xc9, 0x74, 0x04, 0xc6,
        0x04, 0x24, 0x55, 0xe8, 0x04, 0x00, 0x00, 0x00, 0x83, 0xc4, 0x0c, 0xc3, 0xc3,
    ]);
    assert_eq!(args, []);
}

/// The patch sits on the loop's back-edge, so only a path that goes round
/// the loop at least once passes the patched value.
#[test]
fn patch_on_a_loop_back_edge_leaves_no_value() {
    // sub esp,12; mov dword [esp],0x11223344; 1: dec ecx; je 2f;
    // mov byte [esp+1],0x55; jmp 1b; 2: call ext; add esp,12; ret; ext: ret
    let (_, args) = stack_args(vec![
        0x83, 0xec, 0x0c, 0xc7, 0x04, 0x24, 0x44, 0x33, 0x22, 0x11, 0x49, 0x74, 0x07, 0xc6, 0x44,
        0x24, 0x01, 0x55, 0xeb, 0xf6, 0xe8, 0x04, 0x00, 0x00, 0x00, 0x83, 0xc4, 0x0c, 0xc3, 0xc3,
    ]);
    assert_eq!(args, []);
}

#[test]
fn patch_in_a_nested_diamond_leaves_no_value() {
    // sub esp,12; mov dword [esp],0x11223344; test ecx,ecx; je 2f;
    // test edx,edx; je 1f; mov byte [esp+1],0x55; 1: mov eax,1;
    // 2: call ext; add esp,12; ret; ext: ret
    let (_, args) = stack_args(vec![
        0x83, 0xec, 0x0c, 0xc7, 0x04, 0x24, 0x44, 0x33, 0x22, 0x11, 0x85, 0xc9, 0x74, 0x0e, 0x85,
        0xd2, 0x74, 0x05, 0xc6, 0x44, 0x24, 0x01, 0x55, 0xb8, 0x01, 0x00, 0x00, 0x00, 0xe8, 0x04,
        0x00, 0x00, 0x00, 0x83, 0xc4, 0x0c, 0xc3, 0xc3,
    ]);
    assert_eq!(args, []);
}

/// One arm writes the next slot only, so arg 0 is the same on every path.
#[test]
fn one_arm_writing_the_next_slot_keeps_arg_0() {
    // sub esp,12; mov dword [esp],0x11223344; test ecx,ecx; je 1f;
    // mov dword [esp+4],0x99; 1: call ext; add esp,12; ret; ext: ret
    let (function, args) = stack_args(vec![
        0x83, 0xec, 0x0c, 0xc7, 0x04, 0x24, 0x44, 0x33, 0x22, 0x11, 0x85, 0xc9, 0x74, 0x08, 0xc7,
        0x44, 0x24, 0x04, 0x99, 0x00, 0x00, 0x00, 0xe8, 0x04, 0x00, 0x00, 0x00, 0x83, 0xc4, 0x0c,
        0xc3, 0xc3,
    ]);
    let values: Vec<_> = args.iter().map(|&v| function.int_const_u128(v)).collect();
    assert_eq!(values, [Some(0x1122_3344)]);
}
