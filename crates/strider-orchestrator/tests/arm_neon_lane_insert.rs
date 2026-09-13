//! `vmov.<dt> Dn[x], Rt` writes the low `dt` bits of `Rt` into lane `x` and
//! leaves every other bit of `Dn` alone.

mod common;

use common::{Arch, words_image};
use strider_ir::node::NodeKind;
use strider_ir::{IRViewer, IRWalker};

const BASE: u64 = 0x1000;

/// `mvn r2, #0; vmov d1, r2, r2; movw r3, #0x5621; movt r3, #0x1234`
const PRELUDE: [u32; 4] = [0xe3e0_2000, 0xec42_2b11, 0xe305_3621, 0xe341_3234];
/// `vmov r0, r1, d1; bx lr`
const EPILOGUE: [u32; 2] = [0xec51_0b11, 0xe12f_ff1e];

/// `(r0, r1)`, the low and high halves of `d1`.
fn d1_after(arch: Arch, insn: u32) -> (Option<u128>, Option<u128>) {
    let mut words = PRELUDE.to_vec();
    words.push(insn);
    words.extend(EPILOGUE);
    let bytes = words_image(&words, arch.sleigh().endianness());
    let (mut strider, cc) = common::strider_over_bytes(arch, bytes, BASE, None);
    let f = strider
        .analyze(BASE, &cc, &Default::default(), &Default::default(), None)
        .expect("analyze")
        .function;
    let ret = f
        .walk()
        .find(|&n| matches!(f.node_kind(n), NodeKind::Return))
        .expect("one Return");
    let inputs = f.node_inputs(ret);
    (f.int_const_u128(inputs[2]), f.int_const_u128(inputs[3]))
}

#[test]
fn a_lane_insert_shifts_the_element_into_its_lane() {
    let cases = [
        ("vmov.8 d1[7], r3", 0xee61_3b70, (0xffff_ffff, 0x21ff_ffff)),
        ("vmov.8 d1[2], r3", 0xee41_3b50, (0xff21_ffff, 0xffff_ffff)),
        ("vmov.16 d1[2], r3", 0xee21_3b30, (0xffff_ffff, 0xffff_5621)),
        ("vmov.16 d1[1], r3", 0xee01_3b70, (0x5621_ffff, 0xffff_ffff)),
        ("vmov.32 d1[1], r3", 0xee21_3b10, (0xffff_ffff, 0x1234_5621)),
        ("vmov.32 d1[0], r3", 0xee01_3b10, (0x1234_5621, 0xffff_ffff)),
    ];
    let mut wrong = Vec::new();
    for arch in [Arch::Arm, Arch::ArmBe] {
        for (name, insn, (low, high)) in cases {
            let got = d1_after(arch, insn);
            if got != (Some(low), Some(high)) {
                wrong.push(format!("{arch:?} {name}: {got:x?}"));
            }
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}
