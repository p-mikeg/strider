//! MIPS64 under N64: the `d`-form 64-bit shifts, the register-pair return, and
//! the `FREGSIZE 8` float bank.
//!
//! `dsll32` / `dsrl32` / `dsra32` encode a shift amount 32 LOWER than the one
//! they perform, and a 32-bit `sllv` on a 64-bit machine masks its count to 5
//! bits and sign-extends the 32-bit result. The float bank is the N64 half of
//! the ABI split: `f0` is one 8-byte register, not o32's `f0_1` pair, and `f13`
//! is a float argument slot of its own rather than the odd half of `f12`.

mod common;

use common::{returned, words_image};
use strider_ir::node::{NodeKind, ValueId};
use strider_ir::{Function, IRViewer, IntBinaryOp, ValueType};
use strider_target::Endianness;

const BASE: u64 = 0x1000;

/// One word per instruction, as `mips64-linux-gnuabi64-as` emits them. `jr ra`
/// is followed by its delay slot, which retires with the jump.
const WORDS: &[u32] = &[
    0x03e0_0008, // shl32:  jr      ra
    0x0004_103c, //         dsll32  v0, a0, 0
    0x03e0_0008, // shl40:  jr      ra
    0x0004_123c, //         dsll32  v0, a0, 8
    0x03e0_0008, // shl63:  jr      ra
    0x0004_17fc, //         dsll32  v0, a0, 31
    0x03e0_0008, // shr32:  jr      ra
    0x0004_103e, //         dsrl32  v0, a0, 0
    0x03e0_0008, // sar32:  jr      ra
    0x0004_103f, //         dsra32  v0, a0, 0
    0x03e0_0008, // sllv32: jr      ra
    0x00a4_1004, //         sllv    v0, a0, a1
    0x0080_1025, // pair:   move    v0, a0
    0x00a0_1825, //         move    v1, a1
    0x03e0_0008, //         jr      ra
    0x0000_0000, //         nop
    0x03e0_0008, // dadd:   jr      ra
    0x462d_6000, //         add.d   f0, f12, f13
    0x03e0_0008, // fadd:   jr      ra
    0x460d_6000, //         add.s   f0, f12, f13
    0x44a4_0000, // l2d:    dmtc1   a0, f0
    0x03e0_0008, //         jr      ra
    0x46a0_0021, //         cvt.d.l f0, f0
];

const SHL32: u64 = 0x00;
const SHL40: u64 = 0x08;
const SHL63: u64 = 0x10;
const SHR32: u64 = 0x18;
const SAR32: u64 = 0x20;
const SLLV32: u64 = 0x28;
const PAIR: u64 = 0x30;
const DADD: u64 = 0x40;
const FADD: u64 = 0x48;
const L2D: u64 = 0x50;

/// Resolved while the `Sleigh` the assertions compare against is still alive.
const NAMED: &[&str] = &["a0", "a1", "f0", "f2", "f12", "f13"];

struct Vns(Vec<(&'static str, rsleigh::Vn)>);

impl Vns {
    fn get(&self, name: &str) -> rsleigh::Vn {
        self.0
            .iter()
            .find(|(n, _)| *n == name)
            .unwrap_or_else(|| panic!("no register {name}"))
            .1
    }
}

fn analyze(endian: Endianness, offset: u64) -> (Function, Vns) {
    let arch = match endian {
        Endianness::Big => common::Arch::Mips64be,
        Endianness::Little => common::Arch::Mips64le,
    };
    let (mut strider, cc) =
        common::strider_over_bytes(arch, words_image(WORDS, endian), BASE, None);
    let vns = Vns(NAMED
        .iter()
        .map(|&n| {
            let vn = strider
                .sleigh_regs()
                .name_to_vn(n)
                .unwrap_or_else(|| panic!("no register {n}"));
            (n, vn)
        })
        .collect());
    let function = strider
        .analyze(
            BASE + offset,
            &cc,
            &Default::default(),
            &Default::default(),
            None,
        )
        .expect("analyze")
        .function;
    (function, vns)
}

/// The `Return` node's value inputs, in `ret_val_regs` then
/// `ret_val_regs_float` order. Inputs 0 and 1 are the `Control` predecessor and
/// the memory state.
fn producer_kind(f: &Function, v: ValueId) -> &NodeKind {
    f.node_kind(f.producer(v))
}

fn inputs_of(f: &Function, v: ValueId) -> Vec<ValueId> {
    f.node_inputs(f.producer(v)).into_iter().collect()
}

/// The entry-value varnode behind `v`, or `None` when `v` is computed.
fn initial_vn(f: &Function, v: ValueId) -> Option<rsleigh::Vn> {
    match producer_kind(f, v) {
        NodeKind::InitialVar(id) => Some(f.initial_vn(*id)),
        _ => None,
    }
}

/// `(shift op, shift count)` of the value returned in `v0`.
fn v0_shift(f: &Function) -> (IntBinaryOp, u128) {
    let v0 = returned(f)[0];
    let NodeKind::IntBinaryOp(op) = *producer_kind(f, v0) else {
        panic!("v0 is {:?}, not a shift", producer_kind(f, v0));
    };
    let operands = inputs_of(f, v0);
    assert_eq!(
        initial_vn(f, operands[0]).map(|vn| vn.size),
        Some(8),
        "the shifted value is the whole 8-byte a0"
    );
    assert_eq!(
        f.value_type(v0).expect("typed"),
        ValueType::I64,
        "a d-form shift is 64-bit"
    );
    (op, f.int_const_u128(operands[1]).expect("constant count"))
}

#[test]
fn the_d_form_shifts_add_thirty_two_to_their_encoded_count() {
    for endian in [Endianness::Big, Endianness::Little] {
        for (offset, expected) in [
            (SHL32, (IntBinaryOp::ShiftLeft, 32)),
            (SHL40, (IntBinaryOp::ShiftLeft, 40)),
            (SHL63, (IntBinaryOp::ShiftLeft, 63)),
            (SHR32, (IntBinaryOp::ShiftRight, 32)),
            (SAR32, (IntBinaryOp::SShiftRight, 32)),
        ] {
            let (f, _) = analyze(endian, offset);
            assert_eq!(v0_shift(&f), expected, "{endian:?} at {offset:#x}");
        }
    }
}

/// `sllv` is a 32-bit operation on a 64-bit register file: five count bits, a
/// 32-bit shift, and a sign-extended result.
#[test]
fn a_word_shift_masks_its_count_and_sign_extends() {
    let (f, _) = analyze(Endianness::Big, SLLV32);
    let v0 = returned(&f)[0];
    assert!(
        matches!(
            producer_kind(&f, v0),
            NodeKind::Extend(strider_ir::ExtendOp::SignExtend)
        ),
        "v0 is {:?}",
        producer_kind(&f, v0)
    );
    let shift = inputs_of(&f, v0)[0];
    assert_eq!(f.value_type(shift).expect("typed"), ValueType::I32);
    assert!(matches!(
        producer_kind(&f, shift),
        NodeKind::IntBinaryOp(IntBinaryOp::ShiftLeft)
    ));
    let count = inputs_of(&f, shift)[1];
    assert!(
        matches!(
            producer_kind(&f, count),
            NodeKind::IntBinaryOp(IntBinaryOp::And)
        ),
        "count is {:?}, not masked",
        producer_kind(&f, count)
    );
    assert_eq!(
        f.int_const_u128(inputs_of(&f, count)[1]),
        Some(0x1f),
        "five count bits"
    );
}

/// N64 returns a 128-bit aggregate in `v0` / `v1`, and both halves must survive.
#[test]
fn a_pair_return_keeps_both_halves() {
    for endian in [Endianness::Big, Endianness::Little] {
        let (f, vns) = analyze(endian, PAIR);
        let rets = returned(&f);
        assert_eq!(
            initial_vn(&f, rets[0]),
            Some(vns.get("a0")),
            "v0 carries a0"
        );
        assert_eq!(
            initial_vn(&f, rets[1]),
            Some(vns.get("a1")),
            "v1 carries a1"
        );
    }
}

/// `FREGSIZE 8`: `f0` is a whole 8-byte register, so the N64 float return is
/// `f0` / `f2` rather than the o32 `f0_1` / `f2_3` pairs.
#[test]
fn the_float_return_registers_are_eight_bytes_wide() {
    let (f, vns) = analyze(Endianness::Big, SHL32);
    let rets = returned(&f);
    assert_eq!(rets.len(), 4, "v0, v1, f0, f2");
    for (slot, name) in [(2, "f0"), (3, "f2")] {
        let vn = initial_vn(&f, rets[slot]).expect("untouched float return register");
        assert_eq!(vn, vns.get(name));
        assert_eq!(vn.size, 8, "{name} is one 8-byte register under N64");
    }
}

/// `add.d f0, f12, f13` reads two whole 8-byte FPRs: `f13` is N64 float
/// argument 1, not the odd half of a pair.
#[test]
fn a_double_add_reads_two_whole_float_registers() {
    for endian in [Endianness::Big, Endianness::Little] {
        let (f, vns) = analyze(endian, DADD);
        let f0 = returned(&f)[2];
        assert!(matches!(producer_kind(&f, f0), NodeKind::FloatBitsToInt));
        let add = inputs_of(&f, f0)[0];
        assert_eq!(
            *producer_kind(&f, add),
            NodeKind::FloatBinaryOp(strider_ir::FloatBinaryOp::Add)
        );
        assert_eq!(f.value_type(add).expect("typed"), ValueType::F64);
        let mut operands: Vec<rsleigh::Vn> = inputs_of(&f, add)
            .into_iter()
            .map(|v| {
                assert!(matches!(producer_kind(&f, v), NodeKind::IntBitsToFloat));
                initial_vn(&f, inputs_of(&f, v)[0]).expect("an entry FPR")
            })
            .collect();
        operands.sort_by_key(|vn| vn.addr_off);
        assert_eq!(operands, vec![vns.get("f12"), vns.get("f13")]);
        assert!(operands.iter().all(|vn| vn.size == 8));
    }
}

/// `add.s` writes the low 32 bits of an 8-byte FPR and leaves the rest alone.
#[test]
fn a_single_add_preserves_the_upper_half_of_its_destination() {
    let (f, _) = analyze(Endianness::Big, FADD);
    let f0 = returned(&f)[2];
    assert_eq!(
        *producer_kind(&f, f0),
        NodeKind::IntBinaryOp(IntBinaryOp::Or),
        "the write merges into the entry value of f0"
    );
    let merged = inputs_of(&f, f0);
    let kept = merged
        .iter()
        .find(|&&v| {
            matches!(
                producer_kind(&f, v),
                NodeKind::IntBinaryOp(IntBinaryOp::And)
            )
        })
        .copied()
        .expect("the preserved half");
    assert_eq!(
        f.int_const_u128(inputs_of(&f, kept)[1]),
        Some(0xffff_ffff_0000_0000)
    );
    let written = merged
        .iter()
        .find(|&&v| v != kept)
        .copied()
        .expect("the written half");
    let narrow = inputs_of(&f, written)[0];
    assert!(matches!(
        producer_kind(&f, narrow),
        NodeKind::FloatBitsToInt
    ));
    assert_eq!(
        f.value_type(inputs_of(&f, narrow)[0]).expect("typed"),
        ValueType::F32
    );
}

/// `cvt.d.l` converts a 64-bit integer, not the low word of one.
#[test]
fn a_long_to_double_conversion_takes_the_whole_register() {
    let (f, vns) = analyze(Endianness::Big, L2D);
    let f0 = returned(&f)[2];
    assert!(matches!(producer_kind(&f, f0), NodeKind::FloatBitsToInt));
    let cvt = inputs_of(&f, f0)[0];
    assert_eq!(*producer_kind(&f, cvt), NodeKind::IntToFloat);
    assert_eq!(f.value_type(cvt).expect("typed"), ValueType::F64);
    let src = inputs_of(&f, cvt)[0];
    assert_eq!(f.value_type(src).expect("typed"), ValueType::I64);
    assert_eq!(initial_vn(&f, src), Some(vns.get("a0")));
}
