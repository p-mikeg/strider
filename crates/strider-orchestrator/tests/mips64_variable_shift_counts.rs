//! MIPS64 `dsllv` / `dsrlv` / `dsrav` take their count from the low six bits of
//! `rs`. Unmasked, a count of 64 shifts a 64-bit p-code operand to zero instead
//! of leaving the operand alone.

mod common;

use common::words_image;
use strider_ir::node::{NodeKind, ValueId};
use strider_ir::{Function, IRViewer, IRWalker, IntBinaryOp, ValueType};
use strider_target::Endianness;

const BASE: u64 = 0x1000;

/// Two words per function, as `mips64-linux-gnuabi64-as` emits them: the jump
/// and the shift that retires with it in the delay slot.
const WORDS: &[u32] = &[
    0x03e0_0008, // dsllv: jr    ra
    0x00a4_1014, //        dsllv v0, a0, a1
    0x03e0_0008, // dsrlv: jr    ra
    0x00a4_1016, //        dsrlv v0, a0, a1
    0x03e0_0008, // dsrav: jr    ra
    0x00a4_1017, //        dsrav v0, a0, a1
];

const DSLLV: u64 = 0x00;
const DSRLV: u64 = 0x08;
const DSRAV: u64 = 0x10;

fn analyze(endian: Endianness, offset: u64) -> Function {
    let arch = match endian {
        Endianness::Big => common::Arch::Mips64be,
        Endianness::Little => common::Arch::Mips64le,
    };
    let (mut strider, cc) =
        common::strider_over_bytes(arch, words_image(WORDS, endian), BASE, None);
    strider
        .analyze(
            BASE + offset,
            &cc,
            &Default::default(),
            &Default::default(),
            None,
        )
        .expect("analyze")
        .function
}

/// The value returned in `v0`: inputs 0 and 1 of `Return` are control and
/// memory, so the first return-value register follows them.
fn returned_v0(f: &Function) -> ValueId {
    let ret = f
        .walk()
        .find(|&n| matches!(f.node_kind(n), NodeKind::Return))
        .expect("one Return");
    f.node_inputs(ret)[2]
}

fn producer_kind(f: &Function, v: ValueId) -> &NodeKind {
    f.node_kind(f.producer(v))
}

fn inputs_of(f: &Function, v: ValueId) -> Vec<ValueId> {
    f.node_inputs(f.producer(v)).into_iter().collect()
}

fn initial_vn_size(f: &Function, v: ValueId) -> Option<u32> {
    match producer_kind(f, v) {
        NodeKind::InitialVar(id) => Some(f.initial_vn(*id).size),
        _ => None,
    }
}

#[test]
fn a_doubleword_variable_shift_masks_its_count_to_six_bits() {
    for endian in [Endianness::Big, Endianness::Little] {
        for (offset, expected) in [
            (DSLLV, IntBinaryOp::ShiftLeft),
            (DSRLV, IntBinaryOp::ShiftRight),
            (DSRAV, IntBinaryOp::SShiftRight),
        ] {
            let f = analyze(endian, offset);
            let v0 = returned_v0(&f);
            let where_ = format!("{endian:?} at {offset:#x}");

            assert_eq!(
                *producer_kind(&f, v0),
                NodeKind::IntBinaryOp(expected),
                "{where_}: v0 is {:?}",
                producer_kind(&f, v0)
            );
            assert_eq!(
                f.value_type(v0).expect("typed"),
                ValueType::I64,
                "{where_}: a d-form shift is 64-bit"
            );

            let operands = inputs_of(&f, v0);
            assert_eq!(
                initial_vn_size(&f, operands[0]),
                Some(8),
                "{where_}: the shifted value is the whole 8-byte a0"
            );

            let count = operands[1];
            assert!(
                matches!(
                    producer_kind(&f, count),
                    NodeKind::IntBinaryOp(IntBinaryOp::And)
                ),
                "{where_}: count is {:?}, not masked",
                producer_kind(&f, count)
            );
            let mask = inputs_of(&f, count);
            assert_eq!(
                initial_vn_size(&f, mask[0]),
                Some(8),
                "{where_}: the count comes from the whole 8-byte a1"
            );
            assert_eq!(
                f.int_const_u128(mask[1]),
                Some(0x3f),
                "{where_}: six count bits"
            );
        }
    }
}
