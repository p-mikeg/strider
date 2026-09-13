//! x86 high-byte registers `ah` / `bh` / `ch` / `dh` are byte 1 of their
//! container (`ia.sinc`: `define register offset=0 size=1 [ AL AH ... ]`), so a
//! write replaces exactly bits 8..16 and keeps the other bits, and a read is
//! `(r >> 8) & 0xff`. Byte sequences are GNU `as` output, cross-checked with
//! `objdump -D -b binary -mi386:x86-64 -M intel`.

mod common;

use common::Arch;
use strider_ir::IRViewer;

const BASE: u64 = 0x1000;
const SEED: u64 = 0x1122_3344_5566_7788;

/// `movabs <reg>, SEED`.
fn movabs(opcode: u8) -> Vec<u8> {
    let mut v = vec![0x48, opcode];
    v.extend_from_slice(&SEED.to_le_bytes());
    v
}

const MOVABS_RAX: u8 = 0xb8;
const MOVABS_RCX: u8 = 0xb9;
const MOVABS_RDX: u8 = 0xba;
const MOVABS_RBX: u8 = 0xbb;

/// The constant folded into return slot 0 of `arch`'s calling convention.
fn returned_const(arch: Arch, bytes: Vec<u8>) -> u128 {
    let rom: Box<dyn strider_orchestrator::opt::ReadOnlyMemory> = Box::new(
        strider_ir_test_utils::MockRom::raw_bytes(BASE, bytes.clone()),
    );
    let (mut strider, cc) = common::strider_over_bytes(arch, bytes, BASE, Some(rom));
    let f = strider
        .analyze(BASE, &cc, &Default::default(), &Default::default(), None)
        .expect("analyze")
        .function;
    let v = common::returned(&f)[0];
    f.int_const_u128(v).unwrap_or_else(|| {
        panic!(
            "return slot 0 did not fold: {:?}",
            f.node_kind(f.producer(v))
        )
    })
}

fn x64(parts: &[&[u8]]) -> u128 {
    returned_const(Arch::X64, parts.concat())
}

#[test]
fn mov_ah_imm_replaces_only_byte_one() {
    // movabs rax, SEED / mov ah, 0x12 / ret
    assert_eq!(
        x64(&[&movabs(MOVABS_RAX), &[0xb4, 0x12, 0xc3]]),
        0x1122_3344_5566_1288
    );
}

#[test]
fn mov_ah_al_copies_the_low_byte_up() {
    // movabs rax, SEED / mov ah, al / ret
    assert_eq!(
        x64(&[&movabs(MOVABS_RAX), &[0x88, 0xc4, 0xc3]]),
        0x1122_3344_5566_8888
    );
}

#[test]
fn xchg_ah_al_swaps_bytes_zero_and_one() {
    // movabs rax, SEED / xchg ah, al / ret
    assert_eq!(
        x64(&[&movabs(MOVABS_RAX), &[0x86, 0xc4, 0xc3]]),
        0x1122_3344_5566_8877
    );
}

#[test]
fn add_ah_bl_wraps_inside_the_byte() {
    // movabs rax, SEED / movabs rbx, 0xffffffffffffff90 / add ah, bl / ret
    // 0x77 + 0x90 = 0x107: the carry must not reach bit 16.
    let mut rbx = vec![0x48, MOVABS_RBX];
    rbx.extend_from_slice(&0xffff_ffff_ffff_ff90u64.to_le_bytes());
    assert_eq!(
        x64(&[&movabs(MOVABS_RAX), &rbx, &[0x00, 0xdc, 0xc3]]),
        0x1122_3344_5566_0788
    );
}

#[test]
fn movzx_eax_ah_reads_byte_one() {
    // movabs rax, SEED / movzx eax, ah / ret
    assert_eq!(x64(&[&movabs(MOVABS_RAX), &[0x0f, 0xb6, 0xc4, 0xc3]]), 0x77);
}

#[test]
fn mul_cl_writes_ax_and_keeps_the_upper_bits() {
    // movabs rax, 0x1122334455667740 / mov cl, 0x10 / mul cl / ret
    // ax = 0x40 * 0x10 = 0x0400, so ah = 0x04 and al = 0x00.
    let mut rax = vec![0x48, MOVABS_RAX];
    rax.extend_from_slice(&0x1122_3344_5566_7740u64.to_le_bytes());
    assert_eq!(
        x64(&[&rax, &[0xb1, 0x10, 0xf6, 0xe1, 0xc3]]),
        0x1122_3344_5566_0400
    );
}

#[test]
fn bh_ch_dh_writes_replace_only_byte_one() {
    // movabs <r>, SEED / mov <r>h, 0xab / mov rax, <r> / ret
    for (name, seed, write, to_rax) in [
        ("bh", MOVABS_RBX, [0xb7, 0xab], [0x48, 0x89, 0xd8]),
        ("ch", MOVABS_RCX, [0xb5, 0xab], [0x48, 0x89, 0xc8]),
        ("dh", MOVABS_RDX, [0xb6, 0xab], [0x48, 0x89, 0xd0]),
    ] {
        assert_eq!(
            x64(&[&movabs(seed), &write, &to_rax, &[0xc3]]),
            0x1122_3344_5566_ab88,
            "{name}"
        );
    }
}

#[test]
fn bh_ch_dh_reads_take_byte_one() {
    // movabs <r>, SEED / xor eax, eax / mov al, <r>h / ret
    for (name, seed, read) in [
        ("bh", MOVABS_RBX, [0x88, 0xf8]),
        ("ch", MOVABS_RCX, [0x88, 0xe8]),
        ("dh", MOVABS_RDX, [0x88, 0xf0]),
    ] {
        assert_eq!(
            x64(&[&movabs(seed), &[0x31, 0xc0], &read, &[0xc3]]),
            0x77,
            "{name}"
        );
    }
}

#[test]
fn mov_ah_dh_crosses_containers() {
    // movabs rdx, SEED / xor eax, eax / mov ah, dh / ret
    assert_eq!(
        x64(&[&movabs(MOVABS_RDX), &[0x31, 0xc0, 0x88, 0xf4, 0xc3]]),
        0x7700
    );
}

#[test]
fn i386_mov_ah_imm_replaces_only_byte_one() {
    // mov eax, 0x55667788 / mov ah, 0x12 / ret
    assert_eq!(
        returned_const(
            Arch::X86,
            vec![0xb8, 0x88, 0x77, 0x66, 0x55, 0xb4, 0x12, 0xc3]
        ),
        0x5566_1288
    );
}
