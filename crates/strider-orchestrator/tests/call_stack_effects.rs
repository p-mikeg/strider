//! What a call does to the caller's stack pointer and registers beyond the
//! calling convention's defaults: a trap vector pushes no return address, a
//! callee ending in `ret imm16` pops its operand too, and a PC thunk hands its
//! return address back in a register.

mod common;
use common::Arch;
use strider_ir::IRViewer;
use strider_ir::node::NodeKind;

const BASE: u64 = 0x1000;

fn analyze(arch: Arch, bytes: Vec<u8>) -> strider_ir::Function {
    let (mut strider, cc) = common::strider_over_bytes(arch, bytes, BASE, None);
    let result = strider
        .analyze(BASE, &cc, &Default::default(), &Default::default(), None)
        .expect("analyze");
    assert!(result.is_complete(), "no incompleteness is expected here");
    result.function
}

/// The SP-relative offset of the stack slot the first returned value loads.
fn returned_stack_slot(f: &strider_ir::Function) -> i128 {
    let v = common::returned(f)[0];
    let load = f.producer(v);
    assert!(
        matches!(f.node_kind(load), NodeKind::Load(_)),
        "return slot 0 is not a Load: {:?}",
        f.node_kind(load)
    );
    f.stack_offset(load)
        .unwrap_or_else(|| panic!("the returned Load's address is not SP-relative"))
        .1
}

/// A `call rel32` at `at` to `target`.
fn call_rel32(at: u64, target: u64) -> Vec<u8> {
    let rel = (target.wrapping_sub(at + 5)) as u32;
    [&[0xe8][..], &rel.to_le_bytes()].concat()
}

#[test]
fn int_0x80_moves_no_stack_pointer_on_x86() {
    // mov eax,20; int 0x80; mov eax,[esp+4]; ret
    let f = analyze(
        Arch::X86,
        vec![
            0xb8, 0x14, 0, 0, 0, 0xcd, 0x80, 0x8b, 0x44, 0x24, 0x04, 0xc3,
        ],
    );
    assert_eq!(returned_stack_slot(&f), 4);
}

#[test]
fn int_0x80_moves_no_stack_pointer_on_x86_64() {
    // mov eax,39; int 0x80; mov rax,[rsp+8]; ret
    let f = analyze(
        Arch::X64,
        vec![
            0xb8, 0x27, 0, 0, 0, 0xcd, 0x80, 0x48, 0x8b, 0x44, 0x24, 0x08, 0xc3,
        ],
    );
    assert_eq!(returned_stack_slot(&f), 8);
}

#[test]
fn into_moves_no_stack_pointer() {
    // into; mov eax,[esp+4]; ret
    let f = analyze(Arch::X86, vec![0xce, 0x8b, 0x44, 0x24, 0x04, 0xc3]);
    assert_eq!(returned_stack_slot(&f), 4);
}

/// `use(k, m)` of a struct-returning `mk(k)` under gcc -m32 -O2: the caller
/// pushes the hidden result pointer, and `mk` pops it with `ret $4`.
fn sret_caller_then(callee: &[u8]) -> Vec<u8> {
    let head = [
        0x83, 0xec, 0x10, // sub esp,0x10
        0x8d, 0x44, 0x24, 0x04, // lea eax,[esp+4]
        0xff, 0x74, 0x24, 0x14, // push dword [esp+0x14]
        0x50, // push eax
    ];
    let tail = [
        0x8b, 0x44, 0x24, 0x1c, // mov eax,[esp+0x1c]
        0x83, 0xc4, 0x14, // add esp,0x14
        0xc3, // ret
    ];
    let call_at = BASE + head.len() as u64;
    let callee_at = call_at + 5 + tail.len() as u64;
    [&head[..], &call_rel32(call_at, callee_at), &tail, callee].concat()
}

#[test]
fn a_callee_ending_in_ret_imm16_pops_its_operand() {
    // mk: mov eax,[esp+4]; ret 4
    let f = analyze(
        Arch::X86,
        sret_caller_then(&[0x8b, 0x44, 0x24, 0x04, 0xc2, 0x04, 0x00]),
    );
    // `m`, the second argument; the calling convention's plain pop reads `k`.
    assert_eq!(returned_stack_slot(&f), 8);
}

#[test]
fn a_callee_whose_returns_disagree_keeps_the_convention_pop() {
    // test eax,eax; je +3; ret 4; ret
    let f = analyze(
        Arch::X86,
        sret_caller_then(&[0x85, 0xc0, 0x74, 0x03, 0xc2, 0x04, 0x00, 0xc3]),
    );
    assert_eq!(returned_stack_slot(&f), 4);
}

/// i386 PIC `get_msg`: `push ebx; call __x86.get_pc_thunk.bx;
/// add ebx,0x2ffa; lea eax,[ebx-0x2000]; pop ebx; ret`, then `thunk`.
fn pic_caller_then(thunk: &[u8]) -> Vec<u8> {
    let tail = [
        0x81, 0xc3, 0xfa, 0x2f, 0, 0, // add ebx,0x2ffa
        0x8d, 0x83, 0x00, 0xe0, 0xff, 0xff, // lea eax,[ebx-0x2000]
        0x5b, // pop ebx
        0xc3, // ret
    ];
    let call_at = BASE + 1;
    let thunk_at = call_at + 5 + tail.len() as u64;
    [&[0x53][..], &call_rel32(call_at, thunk_at), &tail, thunk].concat()
}

#[test]
fn a_pc_thunk_call_leaves_the_return_address_in_its_register() {
    // __x86.get_pc_thunk.bx: mov ebx,[esp]; ret
    let f = analyze(Arch::X86, pic_caller_then(&[0x8b, 0x1c, 0x24, 0xc3]));
    let v = common::returned(&f)[0];
    // The return address 0x1006, plus 0x2ffa, minus 0x2000.
    assert_eq!(f.int_const_u128(v), Some(0x2000));
}

#[test]
fn an_unrecognised_thunk_body_keeps_the_register_preserved() {
    // mov ebx,[esp]; nop; ret
    let f = analyze(Arch::X86, pic_caller_then(&[0x8b, 0x1c, 0x24, 0x90, 0xc3]));
    let v = common::returned(&f)[0];
    assert_eq!(f.int_const_u128(v), None);
}
