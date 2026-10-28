//! AArch64 conversions that round by FPCR.RMode (bits 22-23: 0 nearest
//! even, 1 toward +inf, 2 toward -inf, 3 toward zero): `fcvt` to a narrower
//! float and `fcvtn` (FPConvert), `scvtf` and `ucvtf` from a general or an FP
//! register, `#fbits` too (FixedToFP), and `bfcvt` and `bfcvtn` to BFloat16
//! (FPConvertBF, a NaN keeping its top fraction bits, quieted).
//! `fp_rounding_mode_is_nearest` takes the mode to be 0; without it each
//! reads FPCR. `fcvtxn` rounds to odd whatever FPCR holds, and `fmov`
//! between a general register and an h register copies the 16 bits.
//!
//! Every encoding is llvm-mc's (`+fullfp16,+bf16`). Each conversion is one
//! instruction and `ret`, lifted once with q0 returned and its source and
//! FPCR as inputs, then evaluated per input against `common::round`.

use crate::common;

use crate::common::Arch;
use crate::common::cone::{Lifted, Val, lift_image};
use crate::common::round::{ARM_MODES, BFLOAT, DOUBLE, Fmt, HALF, Mode, SINGLE};
use strider_orchestrator::opt::AssumptionOptions;

const RET: u32 = 0xd65f_03c0;
const ARCHES: [Arch; 2] = [Arch::Aarch64, Arch::Aarch64Be];

fn lift(arch: Arch, insn: u32, input: &str, claims: &AssumptionOptions) -> Lifted {
    let bytes = common::words_image(&[insn, RET], strider_target::Endianness::Little);
    lift_image(arch, bytes, &["q0"], &[input, "fpcr"], claims)
}

fn int(v: &Val) -> u128 {
    match v {
        Val::Int(x) => *x,
        other => panic!("q0 holds {other:?}"),
    }
}

/// FPCR holding RMode `rm` among other bits.
fn fpcr(rm: u32) -> u128 {
    0x5500_0000_0000_aa00 | u128::from(rm) << 22
}

fn lane(q: u128, width: u32, k: u32) -> u128 {
    q >> (width * k) & ((1 << width) - 1)
}

fn check_float(dst: Fmt, got: u128, want: Option<u128>, what: &str) {
    match want {
        Some(w) => assert_eq!(got, w, "{what}: got {got:#x}, want {w:#x}"),
        None => assert!(dst.is_nan(got), "{what}: {got:#x} is no NaN"),
    }
}

/// fmov (general) between a general register and an h register.
#[test]
fn fmov_between_a_gpr_and_an_h_register_moves_the_bits() {
    let none = AssumptionOptions::none();
    for arch in ARCHES {
        for (asm, word, input, wide) in [
            ("fmov w0, h1", 0x1ee6_0020, "h1", false),
            ("fmov x0, h1", 0x9ee6_0020, "h1", false),
            ("fmov h0, w1", 0x1ee7_0020, "w1", true),
            ("fmov h0, x1", 0x9ee7_0020, "x1", true),
        ] {
            let ret = if wide { "q0" } else { "x0" };
            let bytes = common::words_image(&[word, RET], strider_target::Endianness::Little);
            let l = lift_image(arch, bytes, &[ret], &[input], &none);
            for bits in [
                0x3c00u128,
                0x7e01,
                0xfc00,
                0x0001,
                0x8000,
                0x7bff,
                0xffff_ffff_ffff_3c00,
            ] {
                for fill in [0, u128::MAX] {
                    let got = int(&l.run(&[bits], fill)[0]);
                    assert_eq!(
                        got,
                        bits & 0xffff,
                        "{arch:?} {asm} {bits:#x} fill {fill:#x}"
                    );
                }
            }
        }
    }
}

struct Narrow {
    asm: &'static str,
    word: u32,
    src: Fmt,
    dst: Fmt,
    input: &'static str,
    lanes: u32,
}

const NARROWS: [Narrow; 5] = [
    Narrow {
        asm: "fcvt h0, d1",
        word: 0x1e63_c020,
        src: DOUBLE,
        dst: HALF,
        input: "d1",
        lanes: 1,
    },
    Narrow {
        asm: "fcvt h0, s1",
        word: 0x1e23_c020,
        src: SINGLE,
        dst: HALF,
        input: "s1",
        lanes: 1,
    },
    Narrow {
        asm: "fcvt s0, d1",
        word: 0x1e62_4020,
        src: DOUBLE,
        dst: SINGLE,
        input: "d1",
        lanes: 1,
    },
    Narrow {
        asm: "fcvtn v0.2s, v1.2d",
        word: 0x0e61_6820,
        src: DOUBLE,
        dst: SINGLE,
        input: "q1",
        lanes: 2,
    },
    Narrow {
        asm: "fcvtn v0.4h, v1.4s",
        word: 0x0e21_6820,
        src: SINGLE,
        dst: HALF,
        input: "q1",
        lanes: 4,
    },
];

/// Runs `n` over every input under RMode `rm` (or the claim's 0 when
/// `claimed`), checking each lane against `mode` and the upper bits zero.
fn run_narrow(n: &Narrow, l: &Lifted, rm: u32, mode: Mode, what: &str) {
    let ins = common::round::narrowing_inputs(n.src, n.dst);
    let sw = n.src.bits();
    for group in ins.chunks(n.lanes as usize) {
        let packed = group
            .iter()
            .enumerate()
            .fold(0u128, |acc, (k, &b)| acc | b << (sw * k as u32));
        for fill in [0, u128::MAX] {
            let q = int(&l.run(&[packed, fpcr(rm)], fill)[0]);
            for (k, &b) in group.iter().enumerate() {
                check_float(
                    n.dst,
                    lane(q, n.dst.bits(), k as u32),
                    n.dst.convert(n.src, b, mode),
                    &format!("{what} {} rm {rm} {b:#x}", n.asm),
                );
            }
            assert_eq!(
                q >> (n.dst.bits() * group.len() as u32),
                0,
                "{what} {} zeroes the rest",
                n.asm
            );
        }
    }
}

#[test]
fn narrowing_conversions_round_by_fpcr_rmode() {
    let none = AssumptionOptions::none();
    for arch in ARCHES {
        for n in &NARROWS {
            let l = lift(arch, n.word, n.input, &none);
            for rm in 0..4 {
                run_narrow(n, &l, rm, ARM_MODES[rm as usize], &format!("{arch:?}"));
            }
        }
    }
}

#[test]
fn fcvtxn_rounds_to_odd_whatever_fpcr_holds() {
    let none = AssumptionOptions::none();
    let n = Narrow {
        asm: "fcvtxn s0, d1",
        word: 0x7e61_6820,
        src: DOUBLE,
        dst: SINGLE,
        input: "d1",
        lanes: 1,
    };
    for arch in ARCHES {
        let l = lift(arch, n.word, n.input, &none);
        for rm in 0..4 {
            run_narrow(&n, &l, rm, Mode::Odd, &format!("{arch:?}"));
        }
    }
}

struct IntConv {
    asm: &'static str,
    word: u32,
    signed: bool,
    bits: u32,
    dst: Fmt,
    input: &'static str,
    /// The `#fbits` choices of the fixed-point form, the field at bits 10-15
    /// holding 64 - fbits.
    fbits: &'static [u32],
}

const fn conv(
    asm: &'static str,
    word: u32,
    signed: bool,
    bits: u32,
    dst: Fmt,
    input: &'static str,
) -> IntConv {
    IntConv {
        asm,
        word,
        signed,
        bits,
        dst,
        input,
        fbits: &[0],
    }
}

const W_FBITS: &[u32] = &[1, 20, 32];
const X_FBITS: &[u32] = &[1, 40, 64];

const INT_CONVS: [IntConv; 23] = [
    conv("scvtf h0, w1", 0x1ee2_0020, true, 32, HALF, "w1"),
    conv("scvtf h0, x1", 0x9ee2_0020, true, 64, HALF, "x1"),
    conv("scvtf s0, w1", 0x1e22_0020, true, 32, SINGLE, "w1"),
    conv("scvtf s0, x1", 0x9e22_0020, true, 64, SINGLE, "x1"),
    conv("scvtf d0, w1", 0x1e62_0020, true, 32, DOUBLE, "w1"),
    conv("scvtf d0, x1", 0x9e62_0020, true, 64, DOUBLE, "x1"),
    conv("ucvtf h0, w1", 0x1ee3_0020, false, 32, HALF, "w1"),
    conv("ucvtf h0, x1", 0x9ee3_0020, false, 64, HALF, "x1"),
    conv("ucvtf s0, w1", 0x1e23_0020, false, 32, SINGLE, "w1"),
    conv("ucvtf s0, x1", 0x9e23_0020, false, 64, SINGLE, "x1"),
    conv("ucvtf d0, w1", 0x1e63_0020, false, 32, DOUBLE, "w1"),
    conv("ucvtf d0, x1", 0x9e63_0020, false, 64, DOUBLE, "x1"),
    conv("scvtf s0, s1", 0x5e21_d820, true, 32, SINGLE, "s1"),
    conv("scvtf d0, d1", 0x5e61_d820, true, 64, DOUBLE, "d1"),
    conv("scvtf h0, h1", 0x5e79_d820, true, 16, HALF, "h1"),
    IntConv {
        fbits: W_FBITS,
        ..conv("scvtf h0, w1, #fbits", 0x1ec2_0020, true, 32, HALF, "w1")
    },
    IntConv {
        fbits: X_FBITS,
        ..conv("scvtf h0, x1, #fbits", 0x9ec2_0020, true, 64, HALF, "x1")
    },
    IntConv {
        fbits: W_FBITS,
        ..conv("scvtf s0, w1, #fbits", 0x1e02_0020, true, 32, SINGLE, "w1")
    },
    IntConv {
        fbits: X_FBITS,
        ..conv("scvtf d0, x1, #fbits", 0x9e42_0020, true, 64, DOUBLE, "x1")
    },
    IntConv {
        fbits: W_FBITS,
        ..conv("ucvtf h0, w1, #fbits", 0x1ec3_0020, false, 32, HALF, "w1")
    },
    IntConv {
        fbits: X_FBITS,
        ..conv("ucvtf h0, x1, #fbits", 0x9ec3_0020, false, 64, HALF, "x1")
    },
    IntConv {
        fbits: X_FBITS,
        ..conv("ucvtf s0, x1, #fbits", 0x9e03_0020, false, 64, SINGLE, "x1")
    },
    IntConv {
        fbits: W_FBITS,
        ..conv("ucvtf d0, w1, #fbits", 0x1e43_0020, false, 32, DOUBLE, "w1")
    },
];

fn run_int(c: &IntConv, l: &Lifted, fb: u32, rm: u32, mode: Mode, what: &str) {
    for x in common::round::integer_inputs(c.bits, c.signed, c.dst) {
        let raw = x as u128 & ((1 << c.bits) - 1);
        for fill in [0, u128::MAX] {
            let q = int(&l.run(&[raw, fpcr(rm)], fill)[0]);
            let want = c.dst.round_fixed(x, fb as i32, mode);
            assert_eq!(
                q, want,
                "{what} {} fbits {fb} rm {rm} {x}: got {q:#x}, want {want:#x}",
                c.asm
            );
        }
    }
}

#[test]
fn integer_to_float_conversions_round_by_fpcr_rmode() {
    let none = AssumptionOptions::none();
    for arch in ARCHES {
        for c in &INT_CONVS {
            for &fb in c.fbits {
                let word = if fb == 0 {
                    c.word
                } else {
                    c.word | (64 - fb) << 10
                };
                let l = lift(arch, word, c.input, &none);
                for rm in 0..4 {
                    run_int(c, &l, fb, rm, ARM_MODES[rm as usize], &format!("{arch:?}"));
                }
            }
        }
    }
}

/// A conversion onto its own source register rounds the value it read.
#[test]
fn a_conversion_onto_its_source_reads_it_first() {
    let none = AssumptionOptions::none();
    for arch in ARCHES {
        let n = Narrow {
            asm: "fcvt s1, d1",
            word: 0x1e62_4021,
            src: DOUBLE,
            dst: SINGLE,
            input: "d1",
            lanes: 1,
        };
        let c = conv("scvtf d1, d1", 0x5e61_d821, true, 64, DOUBLE, "d1");
        let lift_q1 = |word: u32| {
            let bytes = common::words_image(&[word, RET], strider_target::Endianness::Little);
            lift_image(arch, bytes, &["q1"], &["d1", "fpcr"], &none)
        };
        let (ln, lc) = (lift_q1(n.word), lift_q1(c.word));
        for rm in 0..4 {
            let mode = ARM_MODES[rm as usize];
            run_narrow(&n, &ln, rm, mode, &format!("{arch:?}"));
            run_int(&c, &lc, 0, rm, mode, &format!("{arch:?}"));
        }
    }
}

/// FPConvertBF: the single rounded to BFloat16 under RMode; a NaN keeps its
/// sign and top fraction bits with the quiet bit set.
fn bfloat(b: u128, mode: Mode) -> u128 {
    if SINGLE.is_nan(b) {
        b >> 16 | 0x40
    } else {
        BFLOAT.convert(SINGLE, b, mode).expect("not a NaN")
    }
}

#[test]
fn bfcvt_rounds_to_bfloat16_by_fpcr_rmode() {
    let none = AssumptionOptions::none();
    let ins = common::round::narrowing_inputs(SINGLE, BFLOAT);
    for arch in ARCHES {
        for (asm, word, input, lanes, upper) in [
            ("bfcvt h0, s1", 0x1e63_4020, "s1", 1, false),
            ("bfcvtn v0.4h, v1.4s", 0x0ea1_6820, "q1", 4, false),
            ("bfcvtn2 v0.8h, v1.4s", 0x4ea1_6820, "q1", 4, true),
        ] {
            let bytes = common::words_image(&[word, RET], strider_target::Endianness::Little);
            let l = lift_image(arch, bytes, &["q0"], &[input, "fpcr", "d0"], &none);
            for rm in 0..4u32 {
                for group in ins.chunks(lanes) {
                    let packed = group
                        .iter()
                        .enumerate()
                        .fold(0u128, |acc, (k, &b)| acc | b << (32 * k));
                    let low = 0x1234_5678_9abc_def0;
                    let q = int(&l.run(&[packed, fpcr(rm), low], 0)[0]);
                    let base = if upper { 4 } else { 0 };
                    for (k, &b) in group.iter().enumerate() {
                        let got = lane(q, 16, base + k as u32);
                        let want = bfloat(b, ARM_MODES[rm as usize]);
                        assert_eq!(got, want, "{arch:?} {asm} rm {rm} {b:#x}: got {got:#x}");
                    }
                    if upper {
                        assert_eq!(q & u128::from(u64::MAX), low, "{arch:?} {asm} keeps d0");
                    } else {
                        assert_eq!(q >> (16 * group.len()), 0, "{arch:?} {asm} zeroes the rest");
                    }
                }
            }
        }
    }
}

/// Under the claim every form reads RMode as 0 whatever FPCR holds.
#[test]
fn the_claim_rounds_to_nearest_whatever_fpcr_holds() {
    let claims = AssumptionOptions::default();
    assert!(claims.fp_rounding_mode_is_nearest);
    for n in &NARROWS {
        let l = lift(Arch::Aarch64, n.word, n.input, &claims);
        for rm in 1..4 {
            run_narrow(n, &l, rm, Mode::Even, "under the claim");
        }
    }
    for c in &INT_CONVS {
        let fb = *c.fbits.last().unwrap();
        let word = if fb == 0 {
            c.word
        } else {
            c.word | (64 - fb) << 10
        };
        let l = lift(Arch::Aarch64, word, c.input, &claims);
        for rm in 1..4 {
            run_int(c, &l, fb, rm, Mode::Even, "under the claim");
        }
    }
}

/// `fpcrRoundingControl` is the last user op on both AArch64 presets, so no
/// other id moved.
#[test]
fn the_rounding_mode_op_is_the_last_user_op() {
    for arch in ARCHES {
        let (strider, _) = common::strider_over_bytes(arch, vec![0; 4], 0x1000, None);
        let names = strider.user_op_names();
        assert_eq!(
            names.last().map(String::as_str),
            Some("fpcrRoundingControl"),
            "{arch:?}"
        );
    }
}
