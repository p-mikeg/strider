//! PowerPC single-precision loads and stores and the word merges (Power ISA
//! 3.0B, Book I 4.6.2, 4.6.3 and 4.6.8).
//!
//! `lfs` and its forms load a single as the double DOUBLE() gives: its
//! value, a NaN keeping its fraction, signalling or quiet. `stfs` and its
//! forms store fS's bits 0:1 and 5:34 when its exponent is above 896 or it
//! is zero, else the fraction denormalised to the single's exponent; they
//! truncate and never round, whatever FPSCR.RN says. Below exponent 874 the
//! word is undefined and these tests do not look at it. `fmrgew` gives
//! FRA[0:31] || FRB[0:31] and `fmrgow` FRA[32:63] || FRB[32:63].

mod common;

use common::Arch;
use common::cone::{Lifted, Val, lift_image, lift_regs};
use strider_orchestrator::opt::AssumptionOptions;

const ALL: [Arch; 4] = [Arch::Ppc32be, Arch::Ppc32le, Arch::Ppc64be, Arch::Ppc64le];
const BLR: u32 = 0x4e80_0020;
/// Where the loads' data sits in the image.
const DATA: usize = 0x40;

fn int(v: &Val) -> u128 {
    match v {
        Val::Int(x) => *x,
        Val::Float(x) => u128::from(x.to_bits()),
        Val::Quad(q) => panic!("a quad {q:#x} in a register"),
    }
}

/// The double `lfs` makes of the single `w`.
fn double_of(w: u32) -> u64 {
    let x = f32::from_bits(w);
    if x.is_nan() {
        u64::from(w & 0x8000_0000) << 32 | 0x7ff0_0000_0000_0000 | u64::from(w & 0x7f_ffff) << 29
    } else {
        f64::from(x).to_bits()
    }
}

/// The word `stfs` stores from the double `d`, `None` where it is undefined.
fn single_of(d: u64) -> Option<u32> {
    let e = d >> 52 & 0x7ff;
    if e > 896 || d << 1 == 0 {
        Some((d >> 32 & 0xc000_0000 | d >> 29 & 0x3fff_ffff) as u32)
    } else if e >= 874 {
        let m = d & ((1 << 52) - 1) | 1 << 52;
        Some((d >> 32 & 0x8000_0000 | m >> (926 - e) & 0x7f_ffff) as u32)
    } else {
        None
    }
}

/// Singles: signed zeros and infinities, quiet and signalling NaNs, the
/// denormal and normal extremes, and plain values.
const SINGLES: [u32; 14] = [
    0x0000_0000,
    0x8000_0000,
    0x3fc0_0000,
    0xbdcc_cccd,
    0x0000_0001,
    0x807f_ffff,
    0x0080_0000,
    0x7f7f_ffff,
    0x7f80_0000,
    0xff80_0000,
    0x7fc0_0000,
    0x7f80_0001,
    0xffbf_ffff,
    0x7fa0_0005,
];

/// Doubles: the singles' doubles, and doubles no single holds: fractions
/// past a single's, which a rounding store would round up, the denormal
/// range's edges, and exponents past a single's.
fn doubles() -> Vec<u64> {
    let mut out: Vec<u64> = SINGLES.iter().map(|&w| double_of(w)).collect();
    out.extend([
        (1.0f64 + 2f64.powi(-24) + 2f64.powi(-40)).to_bits(),
        (-1.0f64 - 2f64.powi(-23) + 2f64.powi(-50)).to_bits(),
        f64::from(f32::MAX).to_bits() + (1 << 28),
        1e300f64.to_bits(),
        (-1e-300f64).to_bits(),
        f64::MAX.to_bits(),
        0x3800_0000_0000_0000 | 0x0008_0000_0000_0000,
        0x3700_0000_0000_0000 | 0x000f_ffff_ffff_ffff,
        0x36a0_0000_0000_0001,
        0x8369_0000_0000_0000,
        0x7ff0_0000_2000_0001,
        0x7ff8_0000_0000_0001,
    ]);
    out
}

/// An image of `words` with the doubleword `data` at [`DATA`].
fn image(arch: Arch, words: &[u32], data: u64) -> Vec<u8> {
    let endian = arch.sleigh().endianness();
    let mut bytes = common::words_image(words, endian);
    bytes.resize(DATA, 0);
    bytes.extend_from_slice(&match endian {
        strider_target::Endianness::Big => data.to_be_bytes(),
        strider_target::Endianness::Little => data.to_le_bytes(),
    });
    bytes
}

/// Each load form of a single at `lis r4, 1` + 0x40 into f1.
const LOADS: [(&str, &[u32]); 4] = [
    ("lfs", &[0x3c80_0001, 0xc024_0040]),
    // addi r4, r4, 64; lfsx f1, 0, r4
    ("lfsx", &[0x3c80_0001, 0x3884_0040, 0x7c20_242e]),
    ("lfsu", &[0x3c80_0001, 0xc424_0040]),
    // li r5, 64; lfsux f1, r4, r5
    ("lfsux", &[0x3c80_0001, 0x38a0_0040, 0x7c24_2c6e]),
];

#[test]
fn single_loads_keep_a_nan_signalling() {
    for arch in ALL {
        let big = arch.sleigh().endianness() == strider_target::Endianness::Big;
        for (name, words) in LOADS {
            let mut words = words.to_vec();
            words.push(BLR);
            for w in SINGLES {
                // The single first in memory: the doubleword's high half when
                // big-endian, its low half when little.
                let data = if big {
                    u64::from(w) << 32
                } else {
                    u64::from(w)
                };
                let lifted = lift_image(
                    arch,
                    image(arch, &words, data),
                    &["f1"],
                    &[],
                    &AssumptionOptions::default(),
                );
                let got = int(&lifted.run(&[], 0)[0]);
                assert_eq!(
                    got,
                    u128::from(double_of(w)),
                    "{arch:?} {name} {w:#010x}: got {got:#x}"
                );
            }
        }
    }
}

/// Each store form of f1 to r1 - 64, read back into r3.
const STORES: [(&str, &[u32]); 4] = [
    // addi r4, r1, -64; stfs f1, 0(r4); lwz r3, 0(r4)
    ("stfs", &[0x3881_ffc0, 0xd024_0000, 0x8064_0000]),
    // addi r4, r1, -64; stfsx f1, 0, r4; lwz r3, 0(r4)
    ("stfsx", &[0x3881_ffc0, 0x7c20_252e, 0x8064_0000]),
    // addi r4, r1, -128; stfsu f1, 64(r4); lwz r3, 0(r4)
    ("stfsu", &[0x3881_ff80, 0xd424_0040, 0x8064_0000]),
    // addi r4, r1, -128; li r5, 64; stfsux f1, r4, r5; lwz r3, 0(r4)
    (
        "stfsux",
        &[0x3881_ff80, 0x38a0_0040, 0x7c24_2d6e, 0x8064_0000],
    ),
];

fn store_lift(arch: Arch, words: &[u32], claims: &AssumptionOptions) -> Lifted {
    let mut words = words.to_vec();
    words.push(BLR);
    lift_regs(arch, &words, &["r3"], &["f1", "r1"], claims)
}

#[test]
fn single_stores_truncate_by_the_isa_formula() {
    for arch in ALL {
        for (name, words) in STORES {
            let lifted = store_lift(arch, words, &AssumptionOptions::default());
            let bad: Vec<String> = doubles()
                .into_iter()
                .filter_map(|d| {
                    let want = single_of(d)?;
                    let got = int(&lifted.run(&[d.into(), 0x8000_0000], 0)[0]);
                    (got != u128::from(want))
                        .then(|| format!("{d:#018x}: got {got:#x}, want {want:#x}"))
                })
                .collect();
            assert!(bad.is_empty(), "{arch:?} {name}: {bad:#?}");
        }
    }
}

/// The stored word reads no FPSCR.RN, with no claim to take it as 0: the
/// all-ones fill sets RN to 3, toward -inf.
#[test]
fn single_stores_ignore_the_rounding_mode() {
    for arch in [Arch::Ppc64le, Arch::Ppc32be] {
        for (name, words) in STORES {
            let lifted = store_lift(arch, words, &AssumptionOptions::none());
            for d in doubles() {
                let Some(want) = single_of(d) else { continue };
                for fill in [0, u128::MAX] {
                    let got = int(&lifted.run(&[d.into(), 0x8000_0000], fill)[0]);
                    assert_eq!(
                        got & 0xffff_ffff,
                        u128::from(want),
                        "{arch:?} {name} {d:#018x} fill {fill:#x}"
                    );
                }
            }
        }
    }
}

#[test]
fn word_merges_take_the_isa_words() {
    let (a, b) = (0x1111_1111_2222_2222u64, 0x3333_3333_4444_4444u64);
    let want = |odd: bool| {
        if odd {
            a << 32 | b & 0xffff_ffff
        } else {
            a & 0xffff_ffff_0000_0000 | b >> 32
        }
    };
    // (word, mnemonic, FRA, FRB, odd); binutils, which llvm-mc lacks.
    let forms = [
        (0xfc22_1f8cu32, "fmrgew f1,f2,f3", "f2", "f3", false),
        (0xfc22_1e8c, "fmrgow f1,f2,f3", "f2", "f3", true),
        (0xfc22_0f8c, "fmrgew f1,f2,f1", "f2", "f1", false),
        (0xfc22_0e8c, "fmrgow f1,f2,f1", "f2", "f1", true),
        (0xfc21_178c, "fmrgew f1,f1,f2", "f1", "f2", false),
        (0xfc21_168c, "fmrgow f1,f1,f2", "f1", "f2", true),
    ];
    for arch in [Arch::Ppc64be, Arch::Ppc64le] {
        for (word, name, ra, rb, odd) in forms {
            let lifted = lift_regs(
                arch,
                &[word, BLR],
                &["f1"],
                &[ra, rb],
                &AssumptionOptions::default(),
            );
            let got = int(&lifted.run(&[a.into(), b.into()], 0)[0]);
            assert_eq!(got, u128::from(want(odd)), "{arch:?} {name}: got {got:#x}");
        }
    }
}
