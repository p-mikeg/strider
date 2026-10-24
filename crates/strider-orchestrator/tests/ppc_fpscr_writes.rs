//! PowerPC FPSCR writes (Power ISA 3.0B, Book I 4.2.2 and 4.6.10): FEX and
//! VX are always the summaries of the bits they cover and cannot be written;
//! FX rises when an instruction changes an exception bit from 0 to 1, but
//! `mtfsf` and `mtfsfi` take field 0's FX and OX from the image; an FP
//! record form sets CR1 to FX, FEX, VX and OX; `mcrfs` clears the exception
//! bits it copies; `fcmpu` and `fcmpo` set all four FPCC bits.
//!
//! Each instruction is lifted alone with every FPSCR bit and CR1-CR3 initial
//! and returned, and checked against a model of the FPSCR as the 32-bit
//! image `mffs` reads, bit 0 (FX) its most significant.

mod common;

use common::Arch;
use common::cone::{Lifted, Val, lift_regs};
use strider_ir_test_utils::Rng;
use strider_orchestrator::opt::AssumptionOptions;

const ARCHES: [Arch; 2] = [Arch::Ppc64le, Arch::Ppc32be];
const BLR: u32 = 0x4e80_0020;

/// The FPSCR bits in image order.
const FPSCR: [&str; 32] = [
    "fp_fx",
    "fp_fex",
    "fp_vx",
    "fp_ox",
    "fp_ux",
    "fp_zx",
    "fp_xx",
    "fp_vxsnan",
    "fp_vxisi",
    "fp_vxidi",
    "fp_vxzdz",
    "fp_vximz",
    "fp_vxvc",
    "fp_fr",
    "fp_fi",
    "fp_c",
    "fp_cc0",
    "fp_cc1",
    "fp_cc2",
    "fp_cc3",
    "fp_reserve1",
    "fp_vxsoft",
    "fp_vxsqrt",
    "fp_vxcvi",
    "fp_ve",
    "fp_oe",
    "fp_ue",
    "fp_ze",
    "fp_xe",
    "fp_ni",
    "fp_rn0",
    "fp_rn1",
];
const CRS: [&str; 3] = ["cr1", "cr2", "cr3"];

const FX: usize = 0;
const FEX: usize = 1;
const VX: usize = 2;
const OX: usize = 3;
const ZX: usize = 5;
const VXSNAN: usize = 7;
/// The exception bits: OX, UX, ZX, XX and the VX* bits.
const EXCEPTIONS: [usize; 13] = [3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 21, 22, 23];
/// The invalid-operation bits VX summarises.
const VX_BITS: [usize; 9] = [7, 8, 9, 10, 11, 12, 21, 22, 23];

fn bit(i: usize) -> u32 {
    1 << (31 - i)
}

fn get(img: u32, i: usize) -> bool {
    img & bit(i) != 0
}

fn put(img: u32, i: usize, v: bool) -> u32 {
    img & !bit(i) | if v { bit(i) } else { 0 }
}

/// `img` with VX and FEX recomputed.
fn summaries(img: u32) -> u32 {
    let img = put(img, VX, VX_BITS.iter().any(|&i| get(img, i)));
    // VX, OX, UX, ZX, XX against VE, OE, UE, ZE, XE.
    let fex = [(2, 24), (3, 25), (4, 26), (5, 27), (6, 28)]
        .iter()
        .any(|&(x, e)| get(img, x) && get(img, e));
    put(img, FEX, fex)
}

/// The FPSCR image and CR1-CR3 a run leaves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct State {
    img: u32,
    crs: [u8; 3],
}

fn lift(arch: Arch, words: &[u32], extra: &[&str]) -> Lifted {
    let regs: Vec<&str> = FPSCR.iter().chain(&CRS).copied().collect();
    let inputs: Vec<&str> = regs.iter().chain(extra).copied().collect();
    let mut all = words.to_vec();
    all.push(BLR);
    lift_regs(arch, &all, &regs, &inputs, &AssumptionOptions::default())
}

fn run(lifted: &Lifted, s: State, extra: &[u128]) -> State {
    let mut vals: Vec<u128> = (0..32).map(|i| u128::from(get(s.img, i))).collect();
    vals.extend(s.crs.map(u128::from));
    vals.extend(extra);
    let out = lifted.run(&vals, 0);
    let int = |v: &Val| match v {
        Val::Int(x) => *x,
        other => panic!("an FPSCR bit or CR field is {other:?}"),
    };
    let img = (0..32).fold(0, |img, i| put(img, i, int(&out[i]) & 1 == 1));
    let crs = [0, 1, 2].map(|k| u8::try_from(int(&out[32 + k])).expect("a 4-bit field"));
    State { img, crs }
}

/// The reserved bit 20.
const RESERVED: usize = 20;

/// Random images with consistent summaries and the reserved bit clear, and
/// random CR fields.
fn states(n: usize, seed: u64) -> Vec<State> {
    let mut rng = Rng(seed);
    (0..n)
        .map(|_| State {
            img: summaries(put(rng.next_u64() as u32, RESERVED, false)),
            crs: [0, 1, 2].map(|_| (rng.next_u64() & 0xf) as u8),
        })
        .collect()
}

/// Checks every state, naming the first mismatch.
fn check(
    name: &str,
    lifted: &Lifted,
    cases: &[State],
    extra: &[u128],
    want: impl Fn(State) -> State,
) {
    for &s in cases {
        let (got, want) = (run(lifted, s, extra), want(s));
        assert_eq!(
            got, want,
            "{name} from {:#010x} {:?}: got {:#010x}, want {:#010x}",
            s.img, s.crs, got.img, want.img
        );
    }
}

fn cr1_of(img: u32) -> u8 {
    u8::try_from(img >> 28).expect("four bits")
}

/// `mtfsb0` and `mtfsb1` set bit BT of the FPSCR. BT 1 and 2 (FEX, VX)
/// cannot be written, and a newly set exception bit raises FX.
#[test]
fn mtfsb_keeps_the_summaries_and_raises_fx_on_a_new_exception() {
    let cases = states(24, 0x5eed_f5b0);
    for arch in ARCHES {
        for bt in 0..32 {
            for one in [false, true] {
                for dot in [false, true] {
                    let word = if one { 0xfc00_004c } else { 0xfc00_008c } | (bt as u32) << 21;
                    let word = word | u32::from(dot);
                    let lifted = lift(arch, &[word], &[]);
                    let name = format!(
                        "{arch:?} mtfsb{}{} {bt}",
                        u8::from(one),
                        if dot { "." } else { "" }
                    );
                    check(&name, &lifted, &cases, &[], |s| {
                        let mut img = s.img;
                        if bt != FEX && bt != VX {
                            if one && EXCEPTIONS.contains(&bt) && !get(img, bt) {
                                img = put(img, FX, true);
                            }
                            img = put(img, bt, one);
                        }
                        let img = summaries(img);
                        let mut crs = s.crs;
                        if dot {
                            crs[0] = cr1_of(img);
                        }
                        State { img, crs }
                    });
                }
            }
        }
    }
}

/// `img` with the fields `mask` covers taken from `new`, as `mtfsf` and
/// `mtfsfi` write them: FEX and VX stay the summaries, field 0's FX and OX
/// come from `new`, and an exception bit outside field 0 that `new` sets
/// from 0 raises FX, the usual rule (Book I 4.6.10's programming note).
fn write_fields(img: u32, new: u32, mask: u32) -> u32 {
    let mut out = img & !mask | new & mask;
    let raised = EXCEPTIONS
        .iter()
        .any(|&i| i != OX && mask & bit(i) != 0 && get(new, i) && !get(img, i));
    if raised {
        out = put(out, FX, true);
    }
    summaries(out)
}

/// `mtfsf FM, f3` writes the FM fields from f3's low word, FEX and VX never.
#[test]
fn mtfsf_recomputes_the_summaries() {
    let cases = states(24, 0x5eed_f5b1);
    let mut rng = Rng(0x5eed_f5b2);
    let words: Vec<u64> = (0..8).map(|_| rng.next_u64()).collect();
    for arch in ARCHES {
        for fm in [0xffu32, 0x80, 0x40, 0x20, 0x01, 0x7f, 0xa5] {
            for dot in [false, true] {
                let lifted = lift(
                    arch,
                    &[0xfc00_058e | fm << 17 | 3 << 11 | u32::from(dot)],
                    &["f3"],
                );
                let mask = (0..8)
                    .filter(|f| fm & (0x80 >> f) != 0)
                    .fold(0u32, |m, f| m | 0xf000_0000 >> (4 * f));
                for &w in &words {
                    check(
                        &format!("{arch:?} mtfsf {fm:#x}"),
                        &lifted,
                        &cases,
                        &[u128::from(w)],
                        |s| {
                            let img = write_fields(s.img, w as u32, mask);
                            let mut crs = s.crs;
                            if dot {
                                crs[0] = cr1_of(img);
                            }
                            State { img, crs }
                        },
                    );
                }
            }
        }
    }
}

/// `mtfsfi BF, U` writes field BF from U.
#[test]
fn mtfsfi_writes_its_field() {
    let mut cases = states(24, 0x5eed_f5b3);
    // each exception bit alone, FX clear, so a field that sets one raises it
    cases.extend(EXCEPTIONS.iter().map(|&i| State {
        img: summaries(bit(i)) & !bit(FX),
        crs: [0, 0, 0],
    }));
    cases.push(State {
        img: 0,
        crs: [0, 0, 0],
    });
    for arch in ARCHES {
        for bf in 0..8u32 {
            for u in [0u32, 1, 2, 4, 8, 6, 9, 0xf] {
                // the record form at fields 0 and 1
                for dot in [false, true].into_iter().filter(|&d| !d || bf < 2) {
                    let lifted = lift(
                        arch,
                        &[0xfc00_010c | bf << 23 | u << 12 | u32::from(dot)],
                        &[],
                    );
                    let mask = 0xf000_0000 >> (4 * bf);
                    check(
                        &format!("{arch:?} mtfsfi {bf}, {u}"),
                        &lifted,
                        &cases,
                        &[],
                        |s| {
                            let img = write_fields(s.img, u << (28 - 4 * bf), mask);
                            let mut crs = s.crs;
                            if dot {
                                crs[0] = cr1_of(img);
                            }
                            State { img, crs }
                        },
                    );
                }
            }
        }
    }
}

/// `mcrfs 2, BFA` copies field BFA into CR2 and clears the exception bits
/// it copied, FX among them; FEX and VX are then the summaries.
#[test]
fn mcrfs_clears_the_exceptions_it_copies() {
    // Each field's exception bits alone, every exception enabled, so the
    // summaries fall to 0 once they are cleared.
    let alone = (0..8).map(|field| State {
        img: summaries(
            EXCEPTIONS
                .iter()
                .filter(|&&i| i / 4 == field)
                .chain(&[24, 25, 26, 27, 28])
                .fold(0, |img, &i| put(img, i, true)),
        ),
        crs: [0; 3],
    });
    let cases: Vec<State> = states(48, 0x5eed_f5b3).into_iter().chain(alone).collect();
    for arch in ARCHES {
        for bfa in 0..8 {
            let lifted = lift(arch, &[0xfc00_0080 | 2 << 23 | (bfa as u32) << 18], &[]);
            check(
                &format!("{arch:?} mcrfs 2,{bfa}"),
                &lifted,
                &cases,
                &[],
                |s| {
                    let field = u8::try_from(s.img >> (28 - 4 * bfa) & 0xf).expect("four bits");
                    let mut img = s.img;
                    for i in 4 * bfa..4 * bfa + 4 {
                        if i == FX || EXCEPTIONS.contains(&i) {
                            img = put(img, i, false);
                        }
                    }
                    let mut crs = s.crs;
                    crs[1] = field;
                    State {
                        img: summaries(img),
                        crs,
                    }
                },
            );
        }
    }
}

/// A record form sets CR1 from the FPSCR it leaves and keeps CR3.
#[test]
fn record_forms_set_cr1_and_keep_cr3() {
    let cases = states(32, 0x5eed_f5b4);
    // fmr. f1, f2; fadd. f1, f2, f3; mtfsb1. 7; mtfsb0. 2 (binutils).
    for word in [0xfc20_1091u32, 0xfc22_182b, 0xfce0_004d, 0xfc40_008d] {
        for arch in ARCHES {
            let lifted = lift(arch, &[word], &["f2", "f3"]);
            for &s in &cases {
                let got = run(
                    &lifted,
                    s,
                    &[1.0f64.to_bits().into(), 2.0f64.to_bits().into()],
                );
                assert_eq!(got.crs[0], cr1_of(got.img), "{arch:?} {word:#x} CR1");
                assert_eq!(got.crs[2], s.crs[2], "{arch:?} {word:#x} CR3");
            }
        }
    }
}

/// `fcmpu` and `fcmpo cr1, f2, f3` set FPCC to the four compare bits they
/// set CR1 to: less, greater, equal, unordered.
#[test]
fn fcmp_sets_all_four_fpcc_bits() {
    let cases = states(8, 0x5eed_f5b5);
    let pairs = [
        (1.0, 2.0),
        (2.0, 1.0),
        (1.0, 1.0),
        (-0.0, 0.0),
        (f64::NAN, 1.0),
        (1.0, f64::NAN),
        (f64::NEG_INFINITY, f64::INFINITY),
    ];
    for arch in ARCHES {
        for word in [0xfc82_1800u32, 0xfc82_1840] {
            let lifted = lift(arch, &[word], &["f2", "f3"]);
            for (a, b) in pairs {
                let extra = [u128::from(f64::to_bits(a)), u128::from(f64::to_bits(b))];
                let want = [a < b, a > b, a == b, a.is_nan() || b.is_nan()]
                    .iter()
                    .fold(0, |c, &f| c << 1 | u8::from(f));
                for &s in &cases {
                    let got = run(&lifted, s, &extra);
                    let fpcc = u8::try_from(got.img >> 12 & 0xf).expect("four bits");
                    assert_eq!(fpcc, want, "{arch:?} {word:#x} {a} {b} FPCC");
                    assert_eq!(got.crs[0], want, "{arch:?} {word:#x} {a} {b} CR1");
                }
            }
        }
    }
}

/// FX rises only when an instruction sets an exception bit that was clear,
/// never from one set before.
#[test]
fn fx_rises_only_on_a_new_exception() {
    let clear = State {
        img: 0,
        crs: [0; 3],
    };
    let with = |bits: &[usize]| State {
        img: summaries(bits.iter().fold(0, |img, &i| put(img, i, true))),
        crs: [0; 3],
    };
    let (one, two, zero, nan) = (1.0f64, 2.0f64, 0.0f64, f64::NAN);
    // (instruction, f2, f3, state before, FX after)
    let fadd = 0xfc22_182au32;
    let fdiv = 0xfc22_1824u32;
    let cases = [
        (fadd, one, two, with(&[OX]), false),
        (fadd, nan, two, clear, true),
        (fadd, nan, two, with(&[VXSNAN]), false),
        (fdiv, one, zero, clear, true),
        (fdiv, one, zero, with(&[ZX]), false),
        (fdiv, one, two, with(&[ZX, OX]), false),
    ];
    for arch in ARCHES {
        for (word, a, b, s, fx) in cases {
            let lifted = lift(arch, &[word], &["f2", "f3"]);
            let got = run(&lifted, s, &[a.to_bits().into(), b.to_bits().into()]);
            assert_eq!(
                get(got.img, FX),
                fx,
                "{arch:?} {word:#x} {a} {b} from {:#010x}",
                s.img
            );
        }
        // mtfsb0 0 clears FX; a later add with OX still set leaves it clear.
        let lifted = lift(arch, &[0xfc00_008c, fadd], &["f2", "f3"]);
        let s = with(&[FX, OX]);
        let got = run(&lifted, s, &[one.to_bits().into(), two.to_bits().into()]);
        assert!(
            !get(got.img, FX),
            "{arch:?} FX re-raised from {:#010x}",
            s.img
        );
    }
}
