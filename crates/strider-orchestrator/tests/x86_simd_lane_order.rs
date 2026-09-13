//! SSE lane order and immediate shift counts, against Intel's definitions:
//! doubleword lane `k` of an XMM register is bits `32k..32k+32`, quadword lane
//! `k` is bits `64k..64k+64`, `pshufd` fills destination lane `i` from source
//! lane `(imm >> 2i) & 3`, and an immediate count past the lane width empties
//! a logical-shift lane and fills an arithmetic-shift lane with its sign.
//! Byte sequences are GNU `as` output, cross-checked with
//! `objdump -D -b binary -mi386:x86-64 -M intel`.

use rsleigh::Sleigh;
use rsleigh::mem_readers::BufMemReader;
use strider_ir::node::NodeKind;
use strider_ir::{IRViewer, IRWalker};
use strider_target::{CallingConvention, SleighArch};

const BASE: u64 = 0x1000;
/// Where the vector `movdqu xmm0, [rip + disp]` loads sits.
const VEC_AT: usize = 0x40;
/// Lanes 0..3 are `0x00112233`, `0x44556677`, `0x8899aabb`, `0xccddeeff`: two
/// positive, two negative.
const VEC: u128 = 0xccdd_eeff_8899_aabb_4455_6677_0011_2233;

const MOVQ_RAX_XMM0: &[u8] = &[0x66, 0x48, 0x0f, 0x7e, 0xc0];
const PEXTRQ_RAX_XMM0_1: &[u8] = &[0x66, 0x48, 0x0f, 0x3a, 0x16, 0xc0, 0x01];
const MOVQ_RAX_XMM1: &[u8] = &[0x66, 0x48, 0x0f, 0x7e, 0xc8];
const PEXTRQ_RAX_XMM1_1: &[u8] = &[0x66, 0x48, 0x0f, 0x3a, 0x16, 0xc8, 0x01];
/// `mov eax, 0xdeadbeef`.
const MOV_EAX_DEADBEEF: &[u8] = &[0xb8, 0xef, 0xbe, 0xad, 0xde];

/// `movdqu xmm0, [rip + VEC]` / `body` / `ret`, then `VEC` at `VEC_AT`, lifted
/// and optimised; the constant folded into `rax`.
fn returned_rax(body: &[&[u8]]) -> u128 {
    const LOAD_LEN: usize = 8;
    let mut bytes = vec![0xf3, 0x0f, 0x6f, 0x05];
    bytes.extend_from_slice(&((VEC_AT - LOAD_LEN) as u32).to_le_bytes());
    for part in body {
        bytes.extend_from_slice(part);
    }
    bytes.push(0xc3);
    assert!(bytes.len() <= VEC_AT, "code overruns the vector");
    bytes.resize(VEC_AT, 0);
    bytes.extend_from_slice(&VEC.to_le_bytes());

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
    let rax = f.node_inputs(ret).into_iter().nth(2).expect("rax slot");
    f.int_const_u128(rax)
        .unwrap_or_else(|| panic!("rax did not fold: {:?}", f.node_kind(f.producer(rax))))
}

/// The whole 128-bit register after `body`, read one quadword lane at a time.
fn xmm_after(body: &[&[u8]], low: &[u8], high: &[u8]) -> u128 {
    let lo = returned_rax(&[body, &[low]].concat());
    let hi = returned_rax(&[body, &[high]].concat());
    assert!(lo <= u128::from(u64::MAX) && hi <= u128::from(u64::MAX));
    lo | (hi << 64)
}

fn xmm0_after(body: &[&[u8]]) -> u128 {
    xmm_after(body, MOVQ_RAX_XMM0, PEXTRQ_RAX_XMM0_1)
}

fn dword_lanes(v: u128) -> [u32; 4] {
    std::array::from_fn(|k| (v >> (32 * k)) as u32)
}

fn from_dword_lanes(lanes: [u32; 4]) -> u128 {
    lanes
        .iter()
        .enumerate()
        .fold(0, |acc, (k, &l)| acc | (u128::from(l) << (32 * k)))
}

fn qword_lanes(v: u128) -> [u64; 2] {
    [v as u64, (v >> 64) as u64]
}

fn from_qword_lanes(lanes: [u64; 2]) -> u128 {
    u128::from(lanes[0]) | (u128::from(lanes[1]) << 64)
}

#[test]
fn pextrd_reads_doubleword_lane_k() {
    for (k, lane) in dword_lanes(VEC).into_iter().enumerate() {
        let pextrd = [0x66, 0x0f, 0x3a, 0x16, 0xc0, k as u8];
        assert_eq!(returned_rax(&[&pextrd]), u128::from(lane), "lane {k}");
    }
}

#[test]
fn pextrq_and_movq_read_quadword_lanes() {
    let [q0, q1] = qword_lanes(VEC);
    assert_eq!(returned_rax(&[MOVQ_RAX_XMM0]), u128::from(q0), "movq");
    let pextrq_0 = [0x66, 0x48, 0x0f, 0x3a, 0x16, 0xc0, 0x00];
    assert_eq!(returned_rax(&[&pextrq_0]), u128::from(q0), "pextrq 0");
    assert_eq!(
        returned_rax(&[PEXTRQ_RAX_XMM0_1]),
        u128::from(q1),
        "pextrq 1"
    );
}

#[test]
fn pinsrd_replaces_only_doubleword_lane_k() {
    for k in 0..4u8 {
        let pinsrd = [0x66, 0x0f, 0x3a, 0x22, 0xc0, k];
        let mut lanes = dword_lanes(VEC);
        lanes[usize::from(k)] = 0xdead_beef;
        assert_eq!(
            xmm0_after(&[MOV_EAX_DEADBEEF, &pinsrd]),
            from_dword_lanes(lanes),
            "lane {k}"
        );
    }
}

#[test]
fn movd_into_xmm_zeroes_the_upper_96_bits() {
    let movd_xmm0_eax = [0x66, 0x0f, 0x6e, 0xc0];
    assert_eq!(xmm0_after(&[MOV_EAX_DEADBEEF, &movd_xmm0_eax]), 0xdead_beef);
}

#[test]
fn movq_into_xmm_zeroes_the_upper_64_bits() {
    // movabs rax, 0x0123456789abcdef / movq xmm0, rax
    let mut movabs = vec![0x48, 0xb8];
    movabs.extend_from_slice(&0x0123_4567_89ab_cdefu64.to_le_bytes());
    let movq_xmm0_rax = [0x66, 0x48, 0x0f, 0x6e, 0xc0];
    assert_eq!(
        xmm0_after(&[&movabs, &movq_xmm0_rax]),
        0x0123_4567_89ab_cdef
    );
}

#[test]
fn pshufd_selects_source_lanes_by_two_bit_fields() {
    let src = dword_lanes(VEC);
    for imm in [0x1bu8, 0x39, 0x00, 0xff, 0xd8, 0x4e] {
        // pshufd xmm1, xmm0, imm
        let pshufd = [0x66, 0x0f, 0x70, 0xc8, imm];
        let want = from_dword_lanes(std::array::from_fn(|i| {
            src[usize::from((imm >> (2 * i)) & 3)]
        }));
        assert_eq!(
            xmm_after(&[&pshufd], MOVQ_RAX_XMM1, PEXTRQ_RAX_XMM1_1),
            want,
            "imm {imm:#04x}"
        );
    }
}

/// `66 0f 72 /r ib` and `66 0f 73 /r ib` with `mod=11`, `rm=xmm0`.
fn shift_imm(opcode: u8, reg: u8, count: u8) -> [u8; 5] {
    [0x66, 0x0f, opcode, 0xc0 | (reg << 3), count]
}

fn psrld(count: u8) -> [u8; 5] {
    shift_imm(0x72, 2, count)
}
fn psrad(count: u8) -> [u8; 5] {
    shift_imm(0x72, 4, count)
}
fn pslld(count: u8) -> [u8; 5] {
    shift_imm(0x72, 6, count)
}
fn psrlq(count: u8) -> [u8; 5] {
    shift_imm(0x73, 2, count)
}
fn psllq(count: u8) -> [u8; 5] {
    shift_imm(0x73, 6, count)
}

fn intel_psrld(count: u8) -> u128 {
    from_dword_lanes(dword_lanes(VEC).map(|l| l.checked_shr(count.into()).unwrap_or(0)))
}
fn intel_pslld(count: u8) -> u128 {
    from_dword_lanes(dword_lanes(VEC).map(|l| l.checked_shl(count.into()).unwrap_or(0)))
}
fn intel_psrad(count: u8) -> u128 {
    from_dword_lanes(dword_lanes(VEC).map(|l| ((l as i32) >> count.min(31)) as u32))
}
fn intel_psrlq(count: u8) -> u128 {
    from_qword_lanes(qword_lanes(VEC).map(|l| l.checked_shr(count.into()).unwrap_or(0)))
}
fn intel_psllq(count: u8) -> u128 {
    from_qword_lanes(qword_lanes(VEC).map(|l| l.checked_shl(count.into()).unwrap_or(0)))
}

#[test]
fn immediate_shift_encodings_match_the_assembler() {
    assert_eq!(psrld(40), [0x66, 0x0f, 0x72, 0xd0, 0x28]);
    assert_eq!(psrad(40), [0x66, 0x0f, 0x72, 0xe0, 0x28]);
    assert_eq!(pslld(33), [0x66, 0x0f, 0x72, 0xf0, 0x21]);
    assert_eq!(psrlq(70), [0x66, 0x0f, 0x73, 0xd0, 0x46]);
    assert_eq!(psllq(70), [0x66, 0x0f, 0x73, 0xf0, 0x46]);
}

#[test]
fn immediate_shifts_past_the_lane_width_saturate() {
    assert_eq!(intel_psrad(40), 0xffff_ffff_ffff_ffff_0000_0000_0000_0000);
    for count in [32u8, 40, 255] {
        assert_eq!(xmm0_after(&[&psrld(count)]), 0, "psrld {count}");
        assert_eq!(xmm0_after(&[&pslld(count)]), 0, "pslld {count}");
        assert_eq!(
            xmm0_after(&[&psrad(count)]),
            intel_psrad(count),
            "psrad {count}"
        );
    }
    for count in [64u8, 70, 255] {
        assert_eq!(xmm0_after(&[&psrlq(count)]), 0, "psrlq {count}");
        assert_eq!(xmm0_after(&[&psllq(count)]), 0, "psllq {count}");
    }
}

#[test]
fn immediate_shifts_within_the_lane_width_shift_each_lane() {
    for count in [0u8, 4, 31] {
        assert_eq!(
            xmm0_after(&[&psrld(count)]),
            intel_psrld(count),
            "psrld {count}"
        );
        assert_eq!(
            xmm0_after(&[&pslld(count)]),
            intel_pslld(count),
            "pslld {count}"
        );
        assert_eq!(
            xmm0_after(&[&psrad(count)]),
            intel_psrad(count),
            "psrad {count}"
        );
    }
    for count in [4u8, 63] {
        assert_eq!(
            xmm0_after(&[&psrlq(count)]),
            intel_psrlq(count),
            "psrlq {count}"
        );
        assert_eq!(
            xmm0_after(&[&psllq(count)]),
            intel_psllq(count),
            "psllq {count}"
        );
    }
}
