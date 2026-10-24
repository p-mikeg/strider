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

/// Ties each way and sign, plain fractions, each destination's edges, the
/// doubles either side of them, and values none holds.
const VALUES: [f64; 36] = [
    5e-324,
    -5e-324,
    -0.9,
    -1.0,
    2147483647.9,
    2147483648.0,
    -2147483648.9,
    -2147483649.0,
    4294967295.9,
    4294967296.0,
    -9_223_372_036_854_775_808.0,
    -9_223_372_036_854_777_856.0,
    9_223_372_036_854_774_784.0,
    18_446_744_073_709_549_568.0,
    -1e300,
    1e300,
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
    image_raw(conv.word, rn, x.to_bits())
}

/// The image for `word` with the doubleword `data` loaded into f1.
fn image_raw(word: u32, rn: Option<u32>, data: u64) -> Vec<u8> {
    let mut words: Vec<u32> = rn
        .map(|rn| vec![0xff80_010c | rn << 12])
        .unwrap_or_default();
    words.extend([0x3d20_0001, 0xc829_0040, word, 0x7c23_0066, 0x4e80_0020]);
    let mut bytes: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
    assert!(bytes.len() <= DATA);
    bytes.resize(DATA, 0);
    bytes.extend_from_slice(&data.to_le_bytes());
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

/// `fpscrRoundingControl` and then `fpscrUndefined` are the last user ops on
/// every PowerPC preset, so no other id moved.
#[test]
fn the_rounding_mode_op_is_the_last_user_op() {
    for arch in [Arch::Ppc32be, Arch::Ppc32le, Arch::Ppc64be, Arch::Ppc64le] {
        let (strider, _) = common::strider_over_bytes(arch, vec![0; 4], BASE, None);
        let names = strider.user_op_names();
        assert_eq!(
            names[names.len() - 2..],
            ["fpscrRoundingControl", "fpscrUndefined"],
            "{arch:?}"
        );
    }
}

/// The float `rn` picks from the floats bracketing an exact value: `floor`
/// the greatest at or below it, `ceil` the least at or above, `nearest` the
/// nearest even. Toward zero takes `floor` for a non-negative value.
fn pick<F>(rn: u32, nearest: F, floor: F, ceil: F, neg: bool) -> F {
    match rn {
        0 => nearest,
        2 => ceil,
        3 => floor,
        _ if neg => ceil,
        _ => floor,
    }
}

fn int_to_f64(rn: u32, x: i128) -> f64 {
    let n = x as f64;
    let exact = |f: f64| f as i128;
    let floor = if exact(n) <= x { n } else { n.next_down() };
    let ceil = if exact(n) >= x { n } else { n.next_up() };
    assert!(exact(floor) <= x && x <= exact(ceil));
    pick(rn, n, floor, ceil, x < 0)
}

fn int_to_f32(rn: u32, x: i128) -> f32 {
    let n = x as f32;
    let exact = |f: f32| f as i128;
    let floor = if exact(n) <= x { n } else { n.next_down() };
    let ceil = if exact(n) >= x { n } else { n.next_up() };
    assert!(exact(floor) <= x && x <= exact(ceil));
    pick(rn, n, floor, ceil, x < 0)
}

fn narrowed(rn: u32, x: f64) -> f32 {
    let n = x as f32;
    if x.is_nan() {
        return n;
    }
    let floor = if f64::from(n) <= x { n } else { n.next_down() };
    let ceil = if f64::from(n) >= x { n } else { n.next_up() };
    pick(rn, n, floor, ceil, x < 0.0)
}

/// Integers each format holds exactly, ties, and values past 2^63, which
/// only the unsigned forms read as positive.
const INTS: [u64; 12] = [
    0,
    1,
    u64::MAX,
    (1 << 53) + 1,
    (1 << 53) + 3,
    (1 << 24) + 1,
    1 << 63,
    (1 << 63) + 1,
    (1 << 63) + 1025,
    u64::MAX - 1024,
    0xffff_ff7f_ffff_ffff,
    12_345_678_901_234_567,
];

/// `fcfid`, `fcfids`, `fcfidu` and `fcfidus` (Power ISA 3.0B 4.6.7.2) read
/// the doubleword as a signed or unsigned integer and round it to a double or
/// a single by RN; a single comes back as the double it widens to.
#[test]
fn integer_to_float_conversions_round_by_fpscr_rn() {
    let none = AssumptionOptions::none();
    let forms: [(&str, u32, bool, bool); 4] = [
        ("fcfid", 0xfc20_0e9c, true, false),
        ("fcfids", 0xec20_0e9c, true, true),
        ("fcfidu", 0xfc20_0f9c, false, false),
        ("fcfidus", 0xec20_0f9c, false, true),
    ];
    for (name, word, signed, single) in forms {
        for bits in INTS {
            let x = if signed {
                i128::from(bits as i64)
            } else {
                i128::from(bits)
            };
            for rn in 0..4 {
                let want = if single {
                    f64::from(int_to_f32(rn, x)).to_bits()
                } else {
                    int_to_f64(rn, x).to_bits()
                };
                let got = common::cone::returned_from_under(
                    Arch::Ppc64le,
                    image_raw(word, Some(rn), bits),
                    BASE,
                    0,
                    &none,
                );
                assert_eq!(
                    got,
                    want,
                    "{name} rn {rn} {x}: got {} want {}",
                    f64::from_bits(got),
                    f64::from_bits(want)
                );
            }
        }
    }
}

/// `frsp` rounds a double to single precision by RN.
#[test]
fn frsp_rounds_by_fpscr_rn() {
    let none = AssumptionOptions::none();
    let values = [
        1.0 + 1.0 / (1u64 << 30) as f64,
        -1.0 - 1.0 / (1u64 << 30) as f64,
        1.0 + 1.0 / (1u64 << 24) as f64,
        0.1,
        -0.1,
        1e39,
        -1e39,
        1e-46,
        -1e-46,
        -0.0,
        f64::INFINITY,
    ];
    for x in values {
        for rn in 0..4 {
            let want = f64::from(narrowed(rn, x)).to_bits();
            let got = common::cone::returned_from_under(
                Arch::Ppc64le,
                image_raw(0xfc20_0818, Some(rn), x.to_bits()),
                BASE,
                0,
                &none,
            );
            assert_eq!(got, want, "frsp rn {rn} {x}");
        }
    }
}

/// `xscvdpspn` reads RN exactly when the claim is off (its value is checked
/// in `ppc_vsx_and_rounding.rs`).
#[test]
fn xscvdpspn_reads_fpscr_rn_only_without_the_claim() {
    // xscvdpspn vs34, vs34; blr, vs34 the vector return register
    let words = [0xf040_142f_u32, 0x4e80_0020];
    let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
    for (claims, reads) in [
        (AssumptionOptions::none(), true),
        (AssumptionOptions::default(), false),
    ] {
        let (mut strider, cc) =
            common::strider_over_bytes(Arch::Ppc64le, bytes.clone(), BASE, None);
        let rn1 = strider.sleigh_regs().name_to_vn("fp_rn1").expect("fp_rn1");
        let opts = strider_orchestrator::AnalyzeOptions {
            assumptions: claims,
            ..Default::default()
        };
        let f = strider
            .analyze(BASE, &cc, &opts, None)
            .expect("analyze")
            .function;
        assert_eq!(
            f.initial_var_value(&rn1).is_some(),
            reads,
            "{}",
            f.to_text(false)
        );
    }
}

/// `mtfsfi 0, U` sets FX and OX from U, and FEX and VX from the bits they
/// summarise (Power ISA 3.0B 4.6.10), here all clear.
#[test]
fn mtfsfi_of_field_zero_recomputes_fex_and_vx() {
    // mtfsfi 1..3, 5..7 to 0 clear every VX and enable bit.
    let clears = [
        0xfc80_010c_u32,
        0xfd00_010c,
        0xfd80_010c,
        0xfe80_010c,
        0xff00_010c,
        0xff80_010c,
    ];
    for (u, want) in [(0b0110u32, 0), (0b1001, 0b1001), (0b1111, 0b1001)] {
        let mut words = clears.to_vec();
        // mtfsfi 0, U; mffs f1; mffprd r3, f1; blr
        words.extend([0xfc00_010c | u << 12, 0xfc20_048e, 0x7c23_0066, 0x4e80_0020]);
        let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
        let got = common::cone::returned_from_under(
            Arch::Ppc64le,
            bytes,
            BASE,
            u128::MAX,
            &AssumptionOptions::none(),
        );
        assert_eq!((got >> 28) & 0xf, want, "mtfsfi 0, {u:#06b}");
    }
}

/// FEX and VX stay the summaries of the bits they cover whatever field
/// `mtfsfi` writes: clearing every VX and enable bit through fields 1-3 and
/// 5-7 clears both, and FX and OX keep their value (all ones on entry).
#[test]
fn mtfsfi_of_any_field_keeps_fex_and_vx_the_summaries() {
    let mut words = vec![
        0xfc80_010c_u32,
        0xfd00_010c,
        0xfd80_010c,
        0xfe80_010c,
        0xff00_010c,
        0xff80_010c,
    ];
    // mffs f1; mffprd r3, f1; blr
    words.extend([0xfc20_048e, 0x7c23_0066, 0x4e80_0020]);
    let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
    let got = common::cone::returned_from_under(
        Arch::Ppc64le,
        bytes,
        BASE,
        u128::MAX,
        &AssumptionOptions::none(),
    );
    assert_eq!((got >> 28) & 0xf, 0b1001);
}

/// FEX is the OR of the enabled exceptions, and FX rises with VX: with every
/// bit set on entry and XE cleared, `fadd` keeps FEX set (four enabled
/// exceptions, which an XOR cancelled); with FX, OX, UX, ZX, XX, FI and VXCVI
/// cleared, an `fctiwz` of the NaN f1 holds raises VXCVI and with it VX and FX. The
/// rounding status ops give 0, so no inexact result raises FX.
#[test]
fn fpscr_summaries_are_ors_and_fx_follows_vx() {
    let run = |words: &[u32]| {
        let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
        common::cone::returned_from_with(
            Arch::Ppc64le,
            bytes,
            BASE,
            u128::MAX,
            &AssumptionOptions::none(),
            &|_| Some(0),
        )
    };
    // mtfsfi 7, 0; fadd f1, f1, f1; mffs f1; mffprd r3, f1; blr
    let fex = run(&[
        0xff80_010c,
        0xfc21_082a,
        0xfc20_048e,
        0x7c23_0066,
        0x4e80_0020,
    ]);
    assert_eq!((fex >> 30) & 1, 1, "FEX");
    // mtfsfi 0, 0; mtfsfi 1, 0; mtfsfi 3, 0; mtfsfi 5, 0; fctiwz f1, f1;
    // mffs f1; mffprd r3, f1; blr
    let fx = run(&[
        0xfc00_010c,
        0xfc80_010c,
        0xfd80_010c,
        0xfe80_010c,
        0xfc20_081e,
        0xfc20_048e,
        0x7c23_0066,
        0x4e80_0020,
    ]);
    assert_eq!((fx >> 29) & 1, 1, "VX");
    assert_eq!((fx >> 31) & 1, 1, "FX");
}
