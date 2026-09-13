//! A flag-setting Thumb-2 logical operation takes `C` from its operand: the
//! shifter's carry-out for a shifted register, `ThumbExpandImm_C`'s for an
//! immediate (Arm ARM `ANDS`/`BICS`/`MOVS`/`MVNS`, encodings T1/T2).

mod common;

use strider_ir::node::NodeKind;
use strider_ir::{IRViewer, IRWalker};

const BASE: u64 = 0x1000;

/// `cmp r3, #1` after `movs r3, #0` borrows, clearing `C`; `cmp r3, #0` sets it.
const CLEAR_C: u16 = 0x2b01;
const SET_C: u16 = 0x2b00;
/// `movw r1, #2` / `movw r1, #3`
const R1_IS_2: [u16; 2] = [0xf240, 0x0102];
const R1_IS_3: [u16; 2] = [0xf240, 0x0103];

/// `movw r1, #v; movs r3, #0; cmp r3, #0|1; <insn>; mov.w r0, #0;
/// adc.w r0, r0, #0; bx lr`, so `r0` is `C` after `insn`.
fn carry_after(r1: [u16; 2], primed: u16, insn: [u16; 2]) -> Option<u128> {
    let halves = [
        r1[0], r1[1], 0x2300, primed, insn[0], insn[1], 0xf04f, 0x0000, 0xf140, 0x0000, 0x4770,
    ];
    let bytes = halves.iter().flat_map(|h| h.to_le_bytes()).collect();
    let (mut strider, cc) = common::strider_over_bytes(common::Arch::ArmThumb, bytes, BASE, None);
    let f = strider
        .analyze(
            BASE | 1,
            &cc,
            &Default::default(),
            &Default::default(),
            None,
        )
        .expect("analyze")
        .function;
    let ret = f
        .walk()
        .find(|&n| matches!(f.node_kind(n), NodeKind::Return))
        .expect("one Return");
    f.int_const_u128(f.node_inputs(ret)[2])
}

#[test]
fn a_flag_setting_logical_operation_takes_carry_from_its_operand() {
    let cases = [
        (
            "ands.w r2, r2, r1, lsr #1",
            [0xea12, 0x0251],
            R1_IS_3,
            CLEAR_C,
            1,
        ),
        (
            "ands.w r2, r2, r1, lsr #1",
            [0xea12, 0x0251],
            R1_IS_2,
            SET_C,
            0,
        ),
        (
            "ands.w r2, r1, #0x80000000",
            [0xf011, 0x4200],
            R1_IS_2,
            CLEAR_C,
            1,
        ),
        (
            "ands.w r2, r1, #0x7f000000",
            [0xf011, 0x42fe],
            R1_IS_2,
            SET_C,
            0,
        ),
        (
            "bics.w r2, r1, #0x80000000",
            [0xf031, 0x4200],
            R1_IS_2,
            CLEAR_C,
            1,
        ),
        (
            "movs.w r2, #0x80000000",
            [0xf05f, 0x4200],
            R1_IS_2,
            CLEAR_C,
            1,
        ),
        (
            "mvns.w r2, #0x80000000",
            [0xf07f, 0x4200],
            R1_IS_2,
            CLEAR_C,
            1,
        ),
        (
            "mvns.w r2, r1, lsr #1",
            [0xea7f, 0x0251],
            R1_IS_3,
            CLEAR_C,
            1,
        ),
        ("mvns.w r2, r1, lsr #1", [0xea7f, 0x0251], R1_IS_2, SET_C, 0),
    ];
    let mut wrong = Vec::new();
    for (name, insn, r1, primed, expected) in cases {
        let got = carry_after(r1, primed, insn);
        if got != Some(expected) {
            wrong.push(format!("{name}, expecting C={expected}: {got:?}"));
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

#[test]
fn an_unshifted_operand_leaves_carry_alone() {
    for (name, insn) in [
        ("ands.w r2, r1, #0xff", [0xf011, 0x02ff]),
        ("movs.w r2, #0xff", [0xf05f, 0x02ff]),
        ("movs.w r2, r1", [0xea5f, 0x0201]),
    ] {
        for (primed, expected) in [(CLEAR_C, 0), (SET_C, 1)] {
            assert_eq!(
                carry_after(R1_IS_2, primed, insn),
                Some(expected),
                "{name} with C primed to {expected}",
            );
        }
    }
}
