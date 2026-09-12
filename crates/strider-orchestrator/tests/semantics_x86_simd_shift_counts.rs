//! x86 SIMD shift-by-count: the count is the whole low quadword of the source,
//! and a count at or past the element width clears the lane (or fills it with
//! the sign bit, for the arithmetic right shift).
//!
//! Both halves of that rule are load-bearing. Reading only the low doubleword
//! turns a huge count into a small one, and truncating the count to the lane
//! width turns it into zero, so either mistake silently leaves the input
//! unshifted.

use rsleigh::Sleigh;
use rsleigh::mem_readers::BufMemReader;
use strider_ir::node::NodeKind;
use strider_ir::{IRViewer, IRWalker};
use strider_target::{CallingConvention, SleighArch};

const BASE: u64 = 0x1000;
/// Where the shifted vector and the count vector sit, reached rip-relative.
const VALUE_AT: usize = 0x20;
const COUNT_AT: usize = 0x30;

const PSRAD: u8 = 0xe2;
const PSRLQ: u8 = 0xd3;
const PSLLQ: u8 = 0xf3;

/// Every doubleword negative, so an over-wide arithmetic shift is visibly
/// different from no shift at all.
const SIGN_LANES: u128 = 0x8000_0000_8000_0000_8000_0000_8000_0000;
const ALL_ONES: u128 = u128::MAX;

/// How the shifted lane is copied into the return register.
#[derive(Clone, Copy)]
enum Extract {
    /// `movq rax, xmm0`: the whole low quadword.
    Quad,
    /// `movd eax, xmm0`: the low doubleword, zero-extended into rax.
    Dword,
}

impl Extract {
    fn bytes(self) -> &'static [u8] {
        match self {
            Extract::Quad => &[0x66, 0x48, 0x0f, 0x7e, 0xc0],
            Extract::Dword => &[0x66, 0x0f, 0x7e, 0xc0],
        }
    }
}

/// `movdqa xmm0, [value]` / `movdqa xmm1, [count]` / `<op> xmm0, xmm1` /
/// `<extract>` / `ret`, then the two operand vectors.
fn image(op: u8, extract: Extract, value: u128, count: u128) -> Vec<u8> {
    let mut v = Vec::new();
    v.extend_from_slice(&[0x66, 0x0f, 0x6f, 0x05]);
    v.extend_from_slice(&0x18u32.to_le_bytes());
    v.extend_from_slice(&[0x66, 0x0f, 0x6f, 0x0d]);
    v.extend_from_slice(&0x20u32.to_le_bytes());
    v.extend_from_slice(&[0x66, 0x0f, op, 0xc1]);
    v.extend_from_slice(extract.bytes());
    v.push(0xc3);
    operands(v, value, count)
}

/// `movdqa xmm0, [value]` / `psllq xmm0, imm8` / `movq rax, xmm0` / `ret`.
fn image_imm(count: u8, value: u128) -> Vec<u8> {
    let mut v = Vec::new();
    v.extend_from_slice(&[0x66, 0x0f, 0x6f, 0x05]);
    v.extend_from_slice(&0x18u32.to_le_bytes());
    v.extend_from_slice(&[0x66, 0x0f, 0x73, 0xf0, count]);
    v.extend_from_slice(Extract::Quad.bytes());
    v.push(0xc3);
    operands(v, value, 0)
}

fn operands(mut v: Vec<u8>, value: u128, count: u128) -> Vec<u8> {
    assert!(v.len() <= VALUE_AT, "code overruns the operand pool");
    v.resize(VALUE_AT, 0);
    v.extend_from_slice(&value.to_le_bytes());
    v.resize(COUNT_AT, 0);
    v.extend_from_slice(&count.to_le_bytes());
    v
}

/// The constant left in `rax`, which is return value slot 0 under System V.
fn returned_rax(bytes: Vec<u8>) -> u128 {
    let arch = SleighArch::x86_64();
    let sleigh = Sleigh::new(
        arch.sla_spec(),
        arch.pspec(),
        BufMemReader::new(bytes.clone(), BASE),
    )
    .expect("sleigh");
    let rom: Box<dyn strider_orchestrator::opt::ReadOnlyMemory> =
        Box::new(strider_ir_test_utils::MockRom::raw_bytes(BASE, bytes));
    let mut strider = strider_orchestrator::Strider::new(arch, sleigh, Some(rom)).expect("strider");
    let cc = CallingConvention::x86_64_systemv()
        .build(strider.sleigh_regs())
        .expect("cc");
    let f = strider
        .analyze(BASE, &cc, &Default::default(), &Default::default(), None)
        .expect("analyze")
        .function;
    let ret = f
        .walk()
        .find(|&n| matches!(f.node_kind(n), NodeKind::Return))
        .expect("one Return");
    // Return inputs: Control, Memory, then one per return register.
    let rax = f.node_inputs(ret).into_iter().nth(2).expect("rax slot");
    f.int_const_u128(rax)
        .unwrap_or_else(|| panic!("rax did not fold: {:?}", f.node_kind(f.producer(rax))))
}

/// `count >= 64` empties a quadword lane whatever the count's magnitude.
#[test]
fn a_quadword_shift_past_the_lane_width_yields_zero() {
    for op in [PSLLQ, PSRLQ] {
        for count in [64u128, 1 << 32, u64::MAX as u128] {
            assert_eq!(
                returned_rax(image(op, Extract::Quad, ALL_ONES, count)),
                0,
                "op {op:#x} count {count:#x}"
            );
        }
    }
    assert_eq!(returned_rax(image_imm(64, ALL_ONES)), 0, "psllq xmm0, 64");
}

/// An in-range count still shifts, so the rule above is not simply "always 0".
#[test]
fn an_in_range_quadword_shift_still_shifts() {
    assert_eq!(
        returned_rax(image(PSRLQ, Extract::Quad, ALL_ONES, 8)),
        0x00ff_ffff_ffff_ffff
    );
    assert_eq!(
        returned_rax(image_imm(8, ALL_ONES)),
        0xffff_ffff_ffff_ff00,
        "psllq xmm0, 8"
    );
}

/// The count is `SRC[63:0]`, not `SRC[31:0]`: a count whose low doubleword is
/// 1 but whose quadword is over 2^32 saturates rather than shifting by one.
#[test]
fn the_shift_count_is_read_from_the_whole_low_quadword() {
    assert_eq!(
        returned_rax(image(
            PSRAD,
            Extract::Dword,
            SIGN_LANES,
            0x0000_0001_0000_0001
        )),
        0xffff_ffff,
        "psrad saturates to the sign bit"
    );
    assert_eq!(
        returned_rax(image(PSRLQ, Extract::Quad, ALL_ONES, 0x0000_0001_0000_0001)),
        0,
        "psrlq empties the lane"
    );
}

/// An arithmetic right shift fills with the sign bit at the lane width, and
/// shifts normally below it.
#[test]
fn a_doubleword_arithmetic_shift_saturates_to_the_sign_bit() {
    assert_eq!(
        returned_rax(image(PSRAD, Extract::Dword, SIGN_LANES, 4)),
        0xf800_0000
    );
    for count in [32u128, 1 << 32] {
        assert_eq!(
            returned_rax(image(PSRAD, Extract::Dword, SIGN_LANES, count)),
            0xffff_ffff,
            "count {count:#x}"
        );
    }
}
