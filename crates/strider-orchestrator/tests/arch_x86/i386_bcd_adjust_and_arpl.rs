//! i386 `daa`, `das` and `arpl` against the SDM pseudocode (Vol. 2A DAA,
//! DAS; Vol. 2A ARPL). All 1024 AL / CF / AF inputs of each decimal adjust
//! and every `arpl` case here were also run natively in 32-bit compatibility
//! mode, matching [`daa`], [`das`] and [`arpl`] exactly.
//!
//! DAS keeps the borrow out of `AL - 6` when the high digit needs no
//! adjustment: AL = 5 with AF set gives CF = 1.

use crate::common;

use strider_ir::IRViewer;

const BASE: u64 = 0x1000;

fn parity_even(x: u8) -> bool {
    x.count_ones().is_multiple_of(2)
}

/// `lahf`'s AH: SF ZF 0 AF 0 PF 1 CF.
fn lahf(al: u8, cf: bool, af: bool) -> u32 {
    u32::from(al & 0x80)
        | u32::from(al == 0) << 6
        | u32::from(af) << 4
        | u32::from(parity_even(al)) << 2
        | 2
        | u32::from(cf)
}

/// SDM DAA: AH:AL as `lahf` / `movzx eax, ax` leave them.
fn daa(mut al: u8, cf: bool, af: bool) -> u32 {
    let (old_al, old_cf) = (al, cf);
    let af = if al & 0xf > 9 || af {
        al = al.wrapping_add(6);
        true
    } else {
        false
    };
    let cf = if old_al > 0x99 || old_cf {
        al = al.wrapping_add(0x60);
        true
    } else {
        false
    };
    u32::from(al) | lahf(al, cf, af) << 8
}

/// SDM DAS. Unlike DAA, the second step has no `ELSE CF := 0`.
fn das(mut al: u8, cf: bool, af: bool) -> u32 {
    let (old_al, old_cf) = (al, cf);
    let mut new_cf = false;
    let af = if al & 0xf > 9 || af {
        new_cf = old_cf || al < 6;
        al = al.wrapping_sub(6);
        true
    } else {
        false
    };
    if old_al > 0x99 || old_cf {
        al = al.wrapping_sub(0x60);
        new_cf = true;
    }
    u32::from(al) | lahf(al, new_cf, af) << 8
}

/// `ZF << 16 | AX` after `arpl ax, dx`.
fn arpl(dst: u16, src: u16) -> u32 {
    if dst & 3 < src & 3 {
        1 << 16 | u32::from(dst & !3 | src & 3)
    } else {
        u32::from(dst)
    }
}

/// The constant left in EAX.
fn returned_eax(mut bytes: Vec<u8>) -> u128 {
    bytes.push(0xc3);
    let (mut strider, cc) = common::strider_over_bytes(common::Arch::X86, bytes, BASE, None);
    let f = strider
        .analyze(BASE, &cc, &Default::default(), None)
        .expect("analyze")
        .function;
    let eax = common::returned(&f)[0];
    f.int_const_u128(eax)
        .unwrap_or_else(|| panic!("eax did not fold: {:?}", common::producer_kind(&f, eax)))
}

/// `mov ah, flags` / `sahf` / `mov al, value` / the adjust / `lahf` /
/// `movzx eax, ax`.
fn adjust_image(op: u8, al: u8, cf: bool, af: bool) -> Vec<u8> {
    let flags = 0x02 | u8::from(cf) | u8::from(af) << 4;
    vec![0xb4, flags, 0x9e, 0xb0, al, op, 0x9f, 0x0f, 0xb7, 0xc0]
}

/// Every high digit under each low-digit class the adjust tests: 0, below 6,
/// 6, 9, and the non-decimal 0xA and 0xF.
fn check_adjust(name: &str, op: u8, reference: fn(u8, bool, bool) -> u32) {
    let values = (0..16u8).flat_map(|hi| [0, 5, 6, 9, 0xa, 0xf].map(|lo| hi << 4 | lo));
    for al in values {
        for cf in [false, true] {
            for af in [false, true] {
                assert_eq!(
                    returned_eax(adjust_image(op, al, cf, af)),
                    u128::from(reference(al, cf, af)),
                    "{name} al {al:#x} cf {cf} af {af}"
                );
            }
        }
    }
}

#[test]
fn daa_matches_the_sdm() {
    check_adjust("daa", 0x27, daa);
}

#[test]
fn das_matches_the_sdm() {
    check_adjust("das", 0x2f, das);
}

/// `mov ax, dst` / `mov dx, src` / `clc` or `stc` / `arpl ax, dx` /
/// `setz cl` / `movzx eax, ax` / `movzx ecx, cl` / `shl ecx, 16` /
/// `or eax, ecx`.
fn arpl_image(dst: u16, src: u16, cf: bool) -> Vec<u8> {
    let mut v = vec![0x66, 0xb8];
    v.extend_from_slice(&dst.to_le_bytes());
    v.extend_from_slice(&[0x66, 0xba]);
    v.extend_from_slice(&src.to_le_bytes());
    v.push(if cf { 0xf9 } else { 0xf8 });
    v.extend_from_slice(&[0x63, 0xd0]);
    v.extend_from_slice(&[0x0f, 0x94, 0xc1, 0x0f, 0xb7, 0xc0, 0x0f, 0xb6, 0xc9]);
    v.extend_from_slice(&[0xc1, 0xe1, 0x10, 0x09, 0xc8]);
    v
}

/// ARPL raises the destination's RPL to the source's and sets ZF exactly
/// then; CF plays no part.
#[test]
fn arpl_adjusts_the_rpl_when_zf_says_so() {
    for dst in [0x1230, 0x1231, 0x1232, 0x1233] {
        for src in [0x10, 0x11, 0x12, 0x13] {
            for cf in [false, true] {
                assert_eq!(
                    returned_eax(arpl_image(dst, src, cf)),
                    u128::from(arpl(dst, src)),
                    "arpl {dst:#x}, {src:#x} cf {cf}"
                );
            }
        }
    }
}
