//! OF after an x86 `shr`, `rcl` or `rcr` whose masked count is 1.
//!
//! SDM Vol. 2B SAL/SAR/SHL/SHR: a one-bit `shr` sets OF to the original
//! operand's most significant bit. RCL/RCR: when `(COUNT & COUNTMASK) = 1`,
//! RCL sets `OF = MSB(DEST) XOR CF` after the rotate and RCR sets it from the
//! operand and CF before it. A zero count leaves the flags alone, and any
//! other count leaves OF undefined, so those cases are not asserted.
//!
//! Every case here was also run natively on x86-64 from the same bytes, and
//! the hardware agreed with [`reference`] on all of them.

mod common;

use strider_ir::node::NodeKind;
use strider_ir::{IRViewer, IRWalker};

const BASE: u64 = 0x1000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Op {
    Shr,
    Rcl,
    Rcr,
}

impl Op {
    fn digit(self) -> u8 {
        match self {
            Op::Shr => 5,
            Op::Rcl => 2,
            Op::Rcr => 3,
        }
    }
}

/// How the count is encoded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Count {
    /// `D0` / `D1`: an implicit 1.
    One,
    /// `C0` / `C1` with an imm8.
    Imm(u8),
    /// `D2` / `D3`: `cl`.
    Cl(u8),
}

/// `op` on `dl` / `dx` / `edx` / `rdx`.
fn insn(op: Op, width: u32, count: Count) -> Vec<u8> {
    let modrm = 0xc0 | op.digit() << 3 | 2;
    let mut v = match width {
        16 => vec![0x66],
        64 => vec![0x48],
        _ => vec![],
    };
    let byte_form = width == 8;
    match count {
        Count::One => v.extend_from_slice(&[if byte_form { 0xd0 } else { 0xd1 }, modrm]),
        Count::Imm(n) => v.extend_from_slice(&[if byte_form { 0xc0 } else { 0xc1 }, modrm, n]),
        Count::Cl(_) => v.extend_from_slice(&[if byte_form { 0xd2 } else { 0xd3 }, modrm]),
    }
    v
}

/// `movabs rdx, value` / `mov ecx, count` / OF set (`mov r8d, 0x7fffffff;
/// add r8d, 1`) or cleared (`xor r8d, r8d`) / `stc` or `clc` / the shift /
/// `seto al` / `movzx eax, al` / `ret`.
fn image(op: Op, width: u32, count: Count, value: u64, cf: bool, stale_of: bool) -> Vec<u8> {
    let cl = match count {
        Count::Cl(n) | Count::Imm(n) => u32::from(n),
        Count::One => 1,
    };
    let mut v = vec![0x48, 0xba];
    v.extend_from_slice(&value.to_le_bytes());
    v.push(0xb9);
    v.extend_from_slice(&cl.to_le_bytes());
    if stale_of {
        v.extend_from_slice(&[0x41, 0xb8, 0xff, 0xff, 0xff, 0x7f, 0x41, 0x83, 0xc0, 0x01]);
    } else {
        v.extend_from_slice(&[0x45, 0x31, 0xc0]);
    }
    v.push(if cf { 0xf9 } else { 0xf8 });
    v.extend_from_slice(&insn(op, width, count));
    v.extend_from_slice(&[0x0f, 0x90, 0xc0, 0x0f, 0xb6, 0xc0, 0xc3]);
    v
}

/// OF per the SDM, or `None` where it is undefined.
fn reference(
    op: Op,
    width: u32,
    count: Count,
    value: u64,
    cf: bool,
    stale_of: bool,
) -> Option<u64> {
    let mask = if width == 64 {
        u64::MAX
    } else {
        (1 << width) - 1
    };
    let msb = |x: u64| x >> (width - 1) & 1;
    let masked = match count {
        Count::One => 1,
        Count::Imm(n) | Count::Cl(n) => n & if width == 64 { 0x3f } else { 0x1f },
    };
    match masked {
        0 => Some(u64::from(stale_of)),
        1 => Some(match op {
            Op::Shr => msb(value),
            Op::Rcl => msb((value << 1 | u64::from(cf)) & mask) ^ msb(value),
            Op::Rcr => msb(value) ^ u64::from(cf),
        }),
        _ => None,
    }
}

/// The constant left in `rax`.
fn returned_rax(bytes: Vec<u8>) -> u128 {
    let (mut strider, cc) = common::strider_over_bytes(common::Arch::X64, bytes, BASE, None);
    let f = strider
        .analyze(BASE, &cc, &Default::default(), None)
        .expect("analyze")
        .function;
    let ret = f
        .walk()
        .find(|&n| matches!(f.node_kind(n), NodeKind::Return))
        .expect("one Return");
    let rax = f.node_inputs(ret).into_iter().nth(2).expect("rax slot");
    f.int_const_u128(rax)
        .unwrap_or_else(|| panic!("rax did not fold: {:?}", f.node_kind(f.producer(rax))))
}

fn check(op: Op, counts: &[Count]) {
    for width in [8, 16, 32, 64] {
        let msb = 1u64 << (width - 1);
        for &count in counts {
            for value in [msb, msb | msb >> 1, msb >> 1, 1] {
                for cf in [false, true] {
                    for stale_of in [false, true] {
                        let Some(want) = reference(op, width, count, value, cf, stale_of) else {
                            continue;
                        };
                        let got = returned_rax(image(op, width, count, value, cf, stale_of));
                        assert_eq!(
                            got,
                            u128::from(want),
                            "{op:?} r{width} {count:?} value {value:#x} cf {cf} stale OF {stale_of}"
                        );
                    }
                }
            }
        }
    }
}

/// `shr r/m, 1` sets OF to the operand's original sign bit.
#[test]
fn a_one_bit_shr_sets_of_to_the_original_sign_bit() {
    check(Op::Shr, &[Count::One]);
}

/// The counted `shr` forms already did, through `shrflags`.
#[test]
fn a_counted_shr_by_one_sets_of_to_the_original_sign_bit() {
    check(Op::Shr, &[Count::Imm(1), Count::Cl(1), Count::Imm(0)]);
}

/// The implicit-1 rotates already set OF.
#[test]
fn a_one_bit_rotate_through_carry_sets_of() {
    check(Op::Rcl, &[Count::One]);
    check(Op::Rcr, &[Count::One]);
}

/// A count of 1 by imm8 or `cl`, including one that only masks to 1 (33 for
/// a doubleword, 65 for a quadword), writes OF; a count of 0 keeps it.
#[test]
fn a_counted_rotate_through_carry_by_one_sets_of() {
    let counts = [
        Count::Imm(1),
        Count::Cl(1),
        Count::Imm(0),
        Count::Cl(0),
        Count::Imm(33),
        Count::Cl(65),
    ];
    check(Op::Rcl, &counts);
    check(Op::Rcr, &counts);
}
