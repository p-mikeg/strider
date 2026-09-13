//! A return instruction is a jump to whatever its target register or stack
//! slot holds. Only one that jumps to the address the function was entered
//! with is a return; any other is an indirect branch and must be reported.

mod common;

use common::Arch;

const BASE: u64 = 0x10_000;

fn le32(words: &[u32]) -> Vec<u8> {
    words.iter().flat_map(|w| w.to_le_bytes()).collect()
}

fn be32(words: &[u32]) -> Vec<u8> {
    words.iter().flat_map(|w| w.to_be_bytes()).collect()
}

fn le16(words: &[u16]) -> Vec<u8> {
    words.iter().flat_map(|w| w.to_le_bytes()).collect()
}

fn analyze(arch: Arch, bytes: Vec<u8>) -> strider_orchestrator::AnalyzeResult {
    let size = bytes.len() as u64;
    // The entry's low bit selects Thumb.
    let entry = if matches!(arch, Arch::ArmThumb) {
        BASE | 1
    } else {
        BASE
    };
    let (mut strider, cc) = common::strider_over_bytes(arch, bytes, BASE, None);
    let lift_opts = strider_orchestrator::LiftOptions {
        cfg: strider_cfg::CfgOptions {
            fn_max_size: Some(size),
            ..Default::default()
        },
        ..Default::default()
    };
    strider
        .analyze(entry, &cc, &lift_opts, &Default::default(), None)
        .expect("analyze")
}

fn unresolved(result: &strider_orchestrator::AnalyzeResult) -> Vec<u64> {
    result
        .unresolved_indirect_branches
        .iter()
        .map(|a| a.machine_addr.addr)
        .collect()
}

/// The return at `BASE + ret_offset` is reported, and nothing else is.
fn assert_reported(arch: Arch, bytes: Vec<u8>, ret_offset: u64, what: &str) {
    let result = analyze(arch, bytes);
    assert_eq!(
        unresolved(&result),
        vec![BASE + ret_offset],
        "{what}: the return is an indirect branch"
    );
    assert!(!result.is_complete(), "{what}");
}

fn assert_complete(arch: Arch, bytes: Vec<u8>, what: &str) {
    let result = analyze(arch, bytes);
    assert!(
        result.is_complete(),
        "{what}: a genuine return is complete; unresolved {:x?}",
        unresolved(&result)
    );
}

#[test]
fn x86_64_returns_through_a_rewritten_slot_are_reported() {
    // push %rsi ; ret
    assert_reported(Arch::X64, vec![0x56, 0xc3], 1, "push %rsi; ret");
    // mov %rsi,(%rsp) ; ret
    assert_reported(
        Arch::X64,
        vec![0x48, 0x89, 0x34, 0x24, 0xc3],
        4,
        "mov %rsi,(%rsp); ret",
    );
}

#[test]
fn x86_64_genuine_returns_stay_complete() {
    // push %rbx ; call ; pop %rbx ; ret
    assert_complete(
        Arch::X64,
        vec![0x53, 0xe8, 0xfa, 0x0f, 0x00, 0x00, 0x5b, 0xc3],
        "a callee-saved push around a call",
    );
    // push $0 ; add $8,%rsp ; ret $8
    assert_complete(
        Arch::X64,
        vec![0x6a, 0x00, 0x48, 0x83, 0xc4, 0x08, 0xc2, 0x08, 0x00],
        "ret imm16 past a discarded push",
    );
}

#[test]
fn arm_returns_through_a_rewritten_link_register_are_reported() {
    // mov lr, r1 ; bx lr
    assert_reported(
        Arch::Arm,
        le32(&[0xe1a0_e001, 0xe12f_ff1e]),
        4,
        "mov lr, r1; bx lr",
    );
    // push {r4, lr} ; str r1, [sp, #4] ; pop {r4, pc}
    assert_reported(
        Arch::Arm,
        le32(&[0xe92d_4010, 0xe58d_1004, 0xe8bd_8010]),
        8,
        "the saved lr slot overwritten",
    );
    // Thumb: mov lr, r1 ; bx lr
    assert_reported(
        Arch::ArmThumb,
        le16(&[0x468e, 0x4770]),
        2,
        "Thumb mov lr, r1; bx lr",
    );
}

#[test]
fn arm_genuine_returns_stay_complete() {
    // push {r4, lr} ; bl ; pop {r4, pc}
    assert_complete(
        Arch::Arm,
        le32(&[0xe92d_4010, 0xeb00_03fd, 0xe8bd_8010]),
        "pop {r4, pc} with the saved lr intact",
    );
    // Thumb: push {r4, lr} ; bl ; pop {r4, pc}
    assert_complete(
        Arch::ArmThumb,
        le16(&[0xb510, 0xf000, 0xfffd, 0xbd10]),
        "Thumb pop {r4, pc}",
    );
    // Thumb: bx lr
    assert_complete(Arch::ArmThumb, le16(&[0x4770]), "Thumb interworking bx lr");
}

#[test]
fn aarch64_returns_through_a_rewritten_link_register_are_reported() {
    // mov x30, x1 ; ret
    assert_reported(
        Arch::Aarch64,
        le32(&[0xaa01_03fe, 0xd65f_03c0]),
        4,
        "mov x30, x1; ret",
    );
    // stp x29, x30, [sp, #-16]! ; str x1, [sp, #8] ; ldp x29, x30, [sp], #16 ; ret
    assert_reported(
        Arch::Aarch64,
        le32(&[0xa9bf_7bfd, 0xf900_07e1, 0xa8c1_7bfd, 0xd65f_03c0]),
        12,
        "the saved x30 slot overwritten",
    );
}

#[test]
fn aarch64_genuine_returns_stay_complete() {
    // stp ; bl ; ldp ; ret
    assert_complete(
        Arch::Aarch64,
        le32(&[0xa9bf_7bfd, 0x9400_03ff, 0xa8c1_7bfd, 0xd65f_03c0]),
        "ldp restoring x30 across a call",
    );
}

#[test]
fn mips_returns_through_a_rewritten_ra_are_reported() {
    // move ra, a1 ; jr ra ; nop
    assert_reported(
        Arch::Mips32be,
        be32(&[0x00a0_f825, 0x03e0_0008, 0x0000_0000]),
        4,
        "move ra, a1; jr ra",
    );
    // addiu sp,-8 ; sw ra,4(sp) ; sw a1,4(sp) ; lw ra,4(sp) ; jr ra ; addiu sp,8
    assert_reported(
        Arch::Mips32be,
        be32(&[
            0x27bd_fff8,
            0xafbf_0004,
            0xafa5_0004,
            0x8fbf_0004,
            0x03e0_0008,
            0x27bd_0008,
        ]),
        16,
        "the saved ra slot overwritten",
    );
}

#[test]
fn mips_genuine_returns_stay_complete() {
    // addiu sp,-8 ; sw ra,4(sp) ; jal ; nop ; lw ra,4(sp) ; jr ra ; addiu sp,8
    assert_complete(
        Arch::Mips32be,
        be32(&[
            0x27bd_fff8,
            0xafbf_0004,
            0x0c00_0403,
            0x0000_0000,
            0x8fbf_0004,
            0x03e0_0008,
            0x27bd_0008,
        ]),
        "jr ra with a delay slot, ra restored across a call",
    );
}

#[test]
fn ppc_returns_through_a_rewritten_link_register_are_reported() {
    // mtlr r4 ; blr
    assert_reported(
        Arch::Ppc32be,
        be32(&[0x7c88_03a6, 0x4e80_0020]),
        4,
        "mtlr r4; blr",
    );
    // mflr r0 ; stw r0,4(r1) ; stwu r1,-16(r1) ; stw r4,20(r1) ; addi r1,r1,16 ;
    // lwz r0,4(r1) ; mtlr r0 ; blr
    assert_reported(
        Arch::Ppc32be,
        be32(&[
            0x7c08_02a6,
            0x9001_0004,
            0x9421_fff0,
            0x9081_0014,
            0x3821_0010,
            0x8001_0004,
            0x7c08_03a6,
            0x4e80_0020,
        ]),
        28,
        "the saved LR slot overwritten",
    );
}

#[test]
fn ppc_genuine_returns_stay_complete() {
    // mflr ; stw ; stwu ; bl ; addi ; lwz ; mtlr ; blr
    assert_complete(
        Arch::Ppc32be,
        be32(&[
            0x7c08_02a6,
            0x9001_0004,
            0x9421_fff0,
            0x4800_0ff5,
            0x3821_0010,
            0x8001_0004,
            0x7c08_03a6,
            0x4e80_0020,
        ]),
        "blr with LR restored across a call",
    );
}
