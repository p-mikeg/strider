//! PowerPC float-to-integer conversions (Power ISA 3.0B, Book I 4.6.7):
//! `fctid`, `fctiw`, `fctidu` and `fctiwu` round by FPSCR.RN (0 nearest
//! even, 1 toward zero, 2 toward +inf, 3 toward -inf), the `z` forms toward
//! zero. A result out of the destination's range saturates to its bound and
//! a NaN gives the most negative value (0 for the unsigned forms), whatever
//! the mode. `fp_rounding_mode_is_nearest` takes RN to be 0; without it each
//! rounding form reads RN.
//!
//! The image is `[mtfsfi 7, rn;] lis r9, 1; lfd f1, 0x40(r9); <conv> f1, f1;
//! mffprd r3, f1; blr` with the double at 0x10040.

mod common;

use common::Arch;
use strider_orchestrator::opt::AssumptionOptions;

const BASE: u64 = 0x10000;
const DATA: usize = 0x40;

#[derive(Clone, Copy)]
struct Conv {
    name: &'static str,
    word: u32,
    /// Rounds by RN; else toward zero.
    by_rn: bool,
    signed: bool,
    bits: u32,
}

const CONVS: [Conv; 8] = [
    Conv {
        name: "fctid",
        word: 0xfc20_0e5c,
        by_rn: true,
        signed: true,
        bits: 64,
    },
    Conv {
        name: "fctiw",
        word: 0xfc20_081c,
        by_rn: true,
        signed: true,
        bits: 32,
    },
    Conv {
        name: "fctidu",
        word: 0xfc20_0f5c,
        by_rn: true,
        signed: false,
        bits: 64,
    },
    Conv {
        name: "fctiwu",
        word: 0xfc20_091c,
        by_rn: true,
        signed: false,
        bits: 32,
    },
    Conv {
        name: "fctidz",
        word: 0xfc20_0e5e,
        by_rn: false,
        signed: true,
        bits: 64,
    },
    Conv {
        name: "fctiwz",
        word: 0xfc20_081e,
        by_rn: false,
        signed: true,
        bits: 32,
    },
    Conv {
        name: "fctiduz",
        word: 0xfc20_0f5e,
        by_rn: false,
        signed: false,
        bits: 64,
    },
    Conv {
        name: "fctiwuz",
        word: 0xfc20_091e,
        by_rn: false,
        signed: false,
        bits: 32,
    },
];

/// Ties each way and sign, plain fractions, each destination's edges and
/// values none holds.
const VALUES: [f64; 20] = [
    2.5,
    -2.5,
    3.5,
    0.5,
    -0.5,
    2.7,
    -2.7,
    -0.0,
    2147483647.5,
    -2147483648.5,
    4294967295.5,
    3e9,
    -3e9,
    9_223_372_036_854_775_808.0,
    1e19,
    1.8446744073709552e19,
    -1e19,
    f64::NAN,
    f64::INFINITY,
    f64::NEG_INFINITY,
];

fn image(conv: Conv, rn: Option<u32>, x: f64) -> Vec<u8> {
    let mut words: Vec<u32> = rn
        .map(|rn| vec![0xff80_010c | rn << 12])
        .unwrap_or_default();
    words.extend([
        0x3d20_0001,
        0xc829_0040,
        conv.word,
        0x7c23_0066,
        0x4e80_0020,
    ]);
    let mut bytes: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
    assert!(bytes.len() <= DATA);
    bytes.resize(DATA, 0);
    bytes.extend_from_slice(&x.to_le_bytes());
    bytes
}

/// The ISA's answer in the low `bits` of the target register.
fn expected(conv: Conv, rn: u32, x: f64) -> u64 {
    let mask = u64::MAX >> (64 - conv.bits);
    let (lo, hi): (i128, i128) = if conv.signed {
        (-(1 << (conv.bits - 1)), (1 << (conv.bits - 1)) - 1)
    } else {
        (0, (1 << conv.bits) - 1)
    };
    if x.is_nan() {
        return if conv.signed { 1 << (conv.bits - 1) } else { 0 };
    }
    let r = match if conv.by_rn { rn } else { 1 } {
        0 => x.round_ties_even(),
        1 => x.trunc(),
        2 => x.ceil(),
        _ => x.floor(),
    };
    // Exact below 2^127; an infinity casts to the i128 bound.
    let i = (r as i128).clamp(lo, hi);
    (i as u64) & mask
}

fn run(conv: Conv, rn: Option<u32>, x: f64, init: u128, claims: &AssumptionOptions) -> u64 {
    let got =
        common::cone::returned_from_under(Arch::Ppc64le, image(conv, rn, x), BASE, init, claims);
    got & (u64::MAX >> (64 - conv.bits))
}

#[test]
fn conversions_round_by_fpscr_rn_and_saturate() {
    let none = AssumptionOptions::none();
    for conv in CONVS {
        for x in VALUES {
            for rn in 0..4 {
                let got = run(conv, Some(rn), x, 0, &none);
                assert_eq!(
                    got,
                    expected(conv, rn, x),
                    "{} mtfsfi rn {rn} {x}",
                    conv.name
                );
            }
            // Every initial register reads `init`: 0 gives RN 0, 1 RN 3.
            for (init, rn) in [(0, 0), (1, 3)] {
                let got = run(conv, None, x, init, &none);
                assert_eq!(
                    got,
                    expected(conv, rn, x),
                    "{} initial rn {rn} {x}",
                    conv.name
                );
            }
        }
    }
}

/// Under the claim the lift reads RN as 0 whatever FPSCR holds.
#[test]
fn the_claim_rounds_to_nearest_whatever_fpscr_holds() {
    let claims = AssumptionOptions::default();
    assert!(claims.fp_rounding_mode_is_nearest);
    for conv in CONVS {
        for x in [2.5, -2.7, 0.5] {
            let got = run(conv, Some(1), x, 0, &claims);
            assert_eq!(
                got,
                expected(conv, 0, x),
                "{} rn 1 under the claim {x}",
                conv.name
            );
        }
    }
}

/// `mtfsfi 7, U; mffs f1; mffprd r3, f1; blr`: FPSCR bits 60-63 read back
/// as U, whatever the rest of FPSCR held.
#[test]
fn mtfsfi_writes_its_immediate() {
    for u in 0..16u32 {
        let words = [0xff80_010c | u << 12, 0xfc20_048e, 0x7c23_0066, 0x4e80_0020];
        let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
        for init in [0, u128::MAX] {
            let got = common::cone::returned_from_under(
                Arch::Ppc64le,
                bytes.clone(),
                BASE,
                init,
                &AssumptionOptions::none(),
            );
            assert_eq!(got & 0xf, u64::from(u), "mtfsfi 7, {u} init {init:#x}");
        }
    }
}

/// `fpscrRoundingControl` is the last user op on every PowerPC preset, so no
/// other id moved.
#[test]
fn the_rounding_mode_op_is_the_last_user_op() {
    for arch in [Arch::Ppc32be, Arch::Ppc32le, Arch::Ppc64be, Arch::Ppc64le] {
        let (strider, _) = common::strider_over_bytes(arch, vec![0; 4], BASE, None);
        let names = strider.user_op_names();
        assert_eq!(
            names.last().map(String::as_str),
            Some("fpscrRoundingControl"),
            "{arch:?}"
        );
    }
}
