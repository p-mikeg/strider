//! x87 80-bit floats on i386: the extended-precision constant, the arithmetic
//! that must not be folded through `f64`, and the `ST0` return.
//!
//! `NodeKind::FloatConst` carries a `u64`, so an F80 cannot be one. The lift
//! keeps the value as a 10-byte `IntConst` under an `IntBitsToFloat`, and the
//! optimiser's float evaluator declines every type but F32 / F64, so an x87
//! expression survives the pipeline rather than collapsing to a double.

mod common;

use common::returned;
use rsleigh::Sleigh;
use rsleigh::mem_readers::BufMemReader;
use strider_ir::node::{NodeKind, ValueId};
use strider_ir::{Function, IRViewer, ValueType};
use strider_target::{CallingConvention, SleighArch};

const BASE: u64 = 0x1000;

/// `1.5L` and `2.5L` as x87 extended-precision bit patterns: a 15-bit biased
/// exponent over an explicit-integer-bit 64-bit significand.
const ONE_POINT_FIVE_F80: u128 = 0x3fff_c000_0000_0000_0000;
const TWO_POINT_FIVE_F80: u128 = 0x4000_a000_0000_0000_0000;
const ONE_POINT_FIVE_F64: u128 = 0x3ff8_0000_0000_0000;
const TWO_POINT_FIVE_F64: u128 = 0x4004_0000_0000_0000;

/// `fld tbyte ds:0x1030` / `ret`.
const LD_ROM: u64 = 0x0000;
/// `fld tbyte ds:0x1030` / `fld tbyte ds:0x1040` / `faddp st(1),st` / `ret`.
const LD_ROM_ADD: u64 = 0x0007;
/// `fld tbyte [esp+4]` / `fld tbyte [esp+0x10]` / `faddp st(1),st` / `ret`.
const LD_ARGS: u64 = 0x0016;
/// `fld qword ds:0x1050` / `fadd qword ds:0x1058` / `ret`.
const D_ADD_ROM: u64 = 0x0021;

fn image() -> Vec<u8> {
    let mut v = Vec::new();
    v.extend_from_slice(&[0xdb, 0x2d, 0x30, 0x10, 0x00, 0x00, 0xc3]);
    v.extend_from_slice(&[0xdb, 0x2d, 0x30, 0x10, 0x00, 0x00]);
    v.extend_from_slice(&[0xdb, 0x2d, 0x40, 0x10, 0x00, 0x00]);
    v.extend_from_slice(&[0xde, 0xc1, 0xc3]);
    v.extend_from_slice(&[0xdb, 0x6c, 0x24, 0x04]);
    v.extend_from_slice(&[0xdb, 0x6c, 0x24, 0x10]);
    v.extend_from_slice(&[0xde, 0xc1, 0xc3]);
    v.extend_from_slice(&[0xdd, 0x05, 0x50, 0x10, 0x00, 0x00]);
    v.extend_from_slice(&[0xdc, 0x05, 0x58, 0x10, 0x00, 0x00]);
    v.push(0xc3);
    v.resize(0x30, 0);
    v.extend_from_slice(&ONE_POINT_FIVE_F80.to_le_bytes()[..10]);
    v.resize(0x40, 0);
    v.extend_from_slice(&TWO_POINT_FIVE_F80.to_le_bytes()[..10]);
    v.resize(0x50, 0);
    v.extend_from_slice(&(ONE_POINT_FIVE_F64 as u64).to_le_bytes());
    v.extend_from_slice(&(TWO_POINT_FIVE_F64 as u64).to_le_bytes());
    v
}

fn analyze(offset: u64) -> (Function, strider_target::BuiltCallingConvention) {
    let arch = SleighArch::x86();
    let bytes = image();
    let sleigh = Sleigh::new(
        arch.sla_spec(),
        arch.pspec(),
        BufMemReader::new(bytes.clone(), BASE),
    )
    .expect("sleigh");
    let rom: Box<dyn strider_orchestrator::opt::ReadOnlyMemory> =
        Box::new(strider_ir_test_utils::MockRom::raw_bytes(BASE, bytes));
    let mut strider = strider_orchestrator::Strider::new(arch, sleigh, Some(rom)).expect("strider");
    let cc = CallingConvention::x86_cdecl()
        .build(strider.sleigh_regs())
        .expect("cdecl cc");
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
    (function, cc)
}

/// The `Return` node's value inputs: `EAX`, `EDX`, then `ST0`, `ST1`, `XMM0`.
/// The value cdecl returns a `long double` in.
fn st0(f: &Function) -> ValueId {
    returned(f)[2]
}

fn producer_kind(f: &Function, v: ValueId) -> &NodeKind {
    f.node_kind(f.producer(v))
}

fn inputs_of(f: &Function, v: ValueId) -> Vec<ValueId> {
    f.node_inputs(f.producer(v)).into_iter().collect()
}

/// Both operands of the F80 add behind `v`, as raw bit patterns.
fn f80_add_operands(f: &Function, v: ValueId) -> Vec<u128> {
    assert!(
        matches!(producer_kind(f, v), NodeKind::FloatBitsToInt),
        "ST0 holds {:?}",
        producer_kind(f, v)
    );
    let add = inputs_of(f, v)[0];
    assert_eq!(
        *producer_kind(f, add),
        NodeKind::FloatBinaryOp(strider_ir::FloatBinaryOp::Add)
    );
    assert_eq!(
        f.value_type(add).expect("typed"),
        ValueType::F80,
        "x87 adds at extended precision"
    );
    let mut bits: Vec<u128> = inputs_of(f, add)
        .into_iter()
        .map(|operand| {
            assert!(matches!(
                producer_kind(f, operand),
                NodeKind::IntBitsToFloat
            ));
            let payload = inputs_of(f, operand)[0];
            assert_eq!(f.value_type(payload).expect("typed"), ValueType::I80);
            f.int_const_u128(payload).expect("a folded F80 constant")
        })
        .collect();
    bits.sort_unstable();
    bits
}

/// `fld tbyte` keeps all ten bytes: `FloatConst`'s `u64` payload cannot hold
/// them, so the value must arrive as a wide `IntConst`.
#[test]
fn an_eighty_bit_constant_survives_as_a_ten_byte_integer() {
    let (f, _) = analyze(LD_ROM);
    let v = st0(&f);
    assert_eq!(f.value_type(v).expect("typed"), ValueType::I80);
    assert!(
        matches!(producer_kind(&f, v), NodeKind::IntConst(_)),
        "ST0 holds {:?}",
        producer_kind(&f, v)
    );
    assert_eq!(f.int_const_u128(v), Some(ONE_POINT_FIVE_F80));
}

/// Two constant operands and an add the optimiser leaves standing: folding it
/// would need F80 host arithmetic, and evaluating it as an `f64` would answer
/// at the wrong precision.
#[test]
fn an_extended_precision_add_is_not_folded() {
    let (f, _) = analyze(LD_ROM_ADD);
    assert_eq!(
        f80_add_operands(&f, st0(&f)),
        vec![ONE_POINT_FIVE_F80, TWO_POINT_FIVE_F80]
    );
}

/// An x87 load off the stack is ten bytes wide, not eight.
#[test]
fn an_eighty_bit_stack_argument_loads_all_ten_bytes() {
    let (f, _) = analyze(LD_ARGS);
    let v = st0(&f);
    assert!(matches!(producer_kind(&f, v), NodeKind::FloatBitsToInt));
    let add = inputs_of(&f, v)[0];
    assert_eq!(f.value_type(add).expect("typed"), ValueType::F80);
    for operand in inputs_of(&f, add) {
        assert!(matches!(
            producer_kind(&f, operand),
            NodeKind::IntBitsToFloat
        ));
        let load = inputs_of(&f, operand)[0];
        assert!(
            matches!(producer_kind(&f, load), NodeKind::Load(_)),
            "operand is {:?}",
            producer_kind(&f, load)
        );
        assert_eq!(f.value_type(load).expect("typed"), ValueType::I80);
    }
}

/// `fadd m64` widens its double operands to the register format and adds
/// there, so the add is F80 even when nothing in the encoding is.
#[test]
fn a_double_operand_is_widened_before_the_x87_add() {
    let (f, _) = analyze(D_ADD_ROM);
    let v = st0(&f);
    assert!(matches!(producer_kind(&f, v), NodeKind::FloatBitsToInt));
    let add = inputs_of(&f, v)[0];
    assert_eq!(f.value_type(add).expect("typed"), ValueType::F80);
    let mut bits: Vec<u128> = inputs_of(&f, add)
        .into_iter()
        .map(|operand| {
            assert_eq!(*producer_kind(&f, operand), NodeKind::FloatToFloat);
            let narrow = inputs_of(&f, operand)[0];
            assert_eq!(f.value_type(narrow).expect("typed"), ValueType::F64);
            let payload = inputs_of(&f, narrow)[0];
            f.int_const_u128(payload).expect("a folded F64 constant")
        })
        .collect();
    bits.sort_unstable();
    assert_eq!(bits, vec![ONE_POINT_FIVE_F64, TWO_POINT_FIVE_F64]);
}

/// cdecl returns a scalar float in `ST0`, the 10-byte x87 top of stack.
#[test]
fn cdecl_returns_a_long_double_in_st0() {
    let (f, cc) = analyze(LD_ROM);
    assert_eq!(returned(&f).len(), 5, "EAX, EDX, ST0, ST1, XMM0");
    assert_eq!(cc.ret_val_regs.len(), 2);
    assert_eq!(cc.ret_val_regs_float.len(), 3);
    assert_eq!(cc.ret_val_regs_float[0].size, 10, "ST0 is 80 bits");
}
