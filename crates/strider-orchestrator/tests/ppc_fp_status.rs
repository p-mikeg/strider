//! The FPSCR status a PowerPC float arithmetic, estimate, rounding or
//! conversion sets (Power ISA 3.0B, Book I 4.4 and each form's Special
//! Registers Altered): exactly the bits it lists, and no other. FR, FI, OX
//! and UX hang on a rounding the specs do not compute, so each comes from an
//! opaque op and is unknown after; the invalid-operation bits and ZX are the
//! operands' own. A sticky bit only ORs in, and FX rises only on an exception
//! bit newly set.

mod common;

use common::Arch;
use common::cone::{Lifted, Val, lift_regs};
use strider_orchestrator::opt::AssumptionOptions;

const BLR: u32 = 0x4e80_0020;
/// `mffs f4`.
const MFFS: u32 = 0xfc80_048e;

/// FPSCR bit numbers, FX bit 0.
const FX: usize = 0;
const OX: usize = 3;
const UX: usize = 4;
const ZX: usize = 5;
const XX: usize = 6;
const VXSNAN: usize = 7;
const VXISI: usize = 8;
const VXIDI: usize = 9;
const VXZDZ: usize = 10;
const VXIMZ: usize = 11;
const VXVC: usize = 12;
const FR: usize = 13;
const FI: usize = 14;
/// FPRF: C, then FPCC (FL, FG, FE, FU).
const C: usize = 15;
const FL: usize = 16;
const FG: usize = 17;
const FE: usize = 18;
const FU: usize = 19;
const VXSOFT: usize = 21;
const VXSQRT: usize = 22;
const VXCVI: usize = 23;

/// The status bits an instruction may set, with their registers.
const STATUS: [(usize, &str); 20] = [
    (OX, "fp_ox"),
    (UX, "fp_ux"),
    (ZX, "fp_zx"),
    (XX, "fp_xx"),
    (VXSNAN, "fp_vxsnan"),
    (VXISI, "fp_vxisi"),
    (VXIDI, "fp_vxidi"),
    (VXZDZ, "fp_vxzdz"),
    (VXIMZ, "fp_vximz"),
    (VXVC, "fp_vxvc"),
    (FR, "fp_fr"),
    (FI, "fp_fi"),
    (C, "fp_c"),
    (FL, "fp_cc0"),
    (FG, "fp_cc1"),
    (FE, "fp_cc2"),
    (FU, "fp_cc3"),
    (VXSOFT, "fp_vxsoft"),
    (VXSQRT, "fp_vxsqrt"),
    (VXCVI, "fp_vxcvi"),
];

fn mtfsb(bt: usize, one: bool) -> u32 {
    (if one { 0xfc00_004c } else { 0xfc00_008c }) | (bt as u32) << 21
}

#[derive(Clone, Copy)]
struct Op {
    name: &'static str,
    word: u32,
    /// Decoded by the 64-bit specs alone.
    wide: bool,
    /// The status bits it may set.
    sets: &'static [usize],
    /// The status bits it clears.
    zeroes: &'static [usize],
}

const fn op(name: &'static str, word: u32, wide: bool, sets: &'static [usize]) -> Op {
    Op {
        name,
        word,
        wide,
        sets,
        zeroes: &[],
    }
}

const ADD: &[usize] = &[FR, FI, OX, UX, XX, VXSNAN, VXISI];
const MUL: &[usize] = &[FR, FI, OX, UX, XX, VXSNAN, VXIMZ];
const DIV: &[usize] = &[FR, FI, OX, UX, ZX, XX, VXSNAN, VXIDI, VXZDZ];
const SQRT: &[usize] = &[FR, FI, XX, VXSNAN, VXSQRT];
const FMA: &[usize] = &[FR, FI, OX, UX, XX, VXSNAN, VXISI, VXIMZ];
const RSP: &[usize] = &[FR, FI, OX, UX, XX, VXSNAN];
const TO_INT: &[usize] = &[FR, FI, XX, VXSNAN, VXCVI];
const TO_INT_Z: &[usize] = &[FI, XX, VXSNAN, VXCVI];
const FROM_INT: &[usize] = &[FR, FI, XX];
/// FR and FI, which the ISA leaves undefined after an estimate, are unknown;
/// an estimate sets no XX.
const RE: &[usize] = &[FR, FI, OX, UX, ZX, VXSNAN];
const RSQRTE: &[usize] = &[FR, FI, ZX, VXSNAN, VXSQRT];

/// Each as `<op> 1, 2, 3` or `<op> 1, 3`, the multiply-adds `<op> 1, 2, 4, 3`
/// (llvm-mc).
const OPS: [Op; 43] = [
    op("fadd", 0xfc22_182a, false, ADD),
    op("fadds", 0xec22_182a, false, ADD),
    op("fsub", 0xfc22_1828, false, ADD),
    op("fsubs", 0xec22_1828, false, ADD),
    op("fmul", 0xfc22_00f2, false, MUL),
    op("fmuls", 0xec22_00f2, false, MUL),
    op("fdiv", 0xfc22_1824, false, DIV),
    op("fdivs", 0xec22_1824, false, DIV),
    op("fsqrt", 0xfc20_182c, false, SQRT),
    op("fsqrts", 0xec20_182c, false, SQRT),
    op("fmadd", 0xfc22_193a, false, FMA),
    op("fmsub", 0xfc22_1938, false, FMA),
    op("fnmadd", 0xfc22_193e, false, FMA),
    op("fnmsub", 0xfc22_193c, false, FMA),
    op("fmadds", 0xec22_193a, false, FMA),
    op("fmsubs", 0xec22_1938, false, FMA),
    op("fnmadds", 0xec22_193e, false, FMA),
    op("fnmsubs", 0xec22_193c, false, FMA),
    op("frsp", 0xfc20_1818, false, RSP),
    op("fctiw", 0xfc20_181c, false, TO_INT),
    op("fctid", 0xfc20_1e5c, false, TO_INT),
    op("fctidu", 0xfc20_1f5c, true, TO_INT),
    op("fctiwu", 0xfc20_191c, true, TO_INT),
    Op {
        zeroes: &[FR],
        ..op("fctiwz", 0xfc20_181e, false, TO_INT_Z)
    },
    Op {
        zeroes: &[FR],
        ..op("fctidz", 0xfc20_1e5e, false, TO_INT_Z)
    },
    Op {
        zeroes: &[FR],
        ..op("fctiduz", 0xfc20_1f5e, true, TO_INT_Z)
    },
    Op {
        zeroes: &[FR],
        ..op("fctiwuz", 0xfc20_191e, true, TO_INT_Z)
    },
    op("fcfid", 0xfc20_1e9c, false, FROM_INT),
    op("fcfidu", 0xfc20_1f9c, true, FROM_INT),
    op("fcfids", 0xec20_1e9c, true, FROM_INT),
    op("fcfidus", 0xec20_1f9c, true, FROM_INT),
    op("fres", 0xec20_1830, false, RE),
    op("fre", 0xfc20_1830, true, RE),
    op("frsqrte", 0xfc20_1834, false, RSQRTE),
    op("frsqrtes", 0xec20_1834, true, RSQRTE),
    Op {
        zeroes: &[FR, FI],
        ..op("frin", 0xfc20_1b10, true, &[VXSNAN])
    },
    Op {
        zeroes: &[FR, FI],
        ..op("friz", 0xfc20_1b50, true, &[VXSNAN])
    },
    Op {
        zeroes: &[FR, FI],
        ..op("frip", 0xfc20_1b90, true, &[VXSNAN])
    },
    Op {
        zeroes: &[FR, FI],
        ..op("frim", 0xfc20_1bd0, true, &[VXSNAN])
    },
    op("xsadddp", 0xf022_1900, true, ADD),
    op("fadd.", 0xfc22_182b, false, ADD),
    // `fcmpu cr1, f2, f3` and `fcmpo cr1, f2, f3`
    op("fcmpu", 0xfc82_1800, false, &[VXSNAN]),
    op("fcmpo", 0xfc82_1840, false, &[VXSNAN, VXVC]),
];

/// The FPRF bits `op` writes: FPCC alone for a compare, else all five
/// (left undefined, so unknown, by a conversion to an integer).
fn fprf_of(op: Op) -> &'static [usize] {
    if op.name.starts_with("fcmp") {
        &[FL, FG, FE, FU]
    } else {
        &[C, FL, FG, FE, FU]
    }
}

fn arches(op: Op) -> &'static [Arch] {
    if op.wide {
        &[Arch::Ppc64le, Arch::Ppc64be]
    } else {
        &[Arch::Ppc64le, Arch::Ppc32be]
    }
}

fn lift(arch: Arch, words: &[u32], rets: &[&str], inputs: &[&str]) -> Lifted {
    let mut all = words.to_vec();
    all.push(BLR);
    lift_regs(arch, &all, rets, inputs, &AssumptionOptions::default())
}

/// With every status bit written to `one` first, each one the op sets is
/// unknown after (or still set, a sticky bit set before), each it clears is
/// clear, and each other keeps `one`. FPRF is the unknown result's class. Written to 1, then FX cleared, nothing
/// the op does raises FX.
#[test]
fn each_op_sets_exactly_its_status_bits() {
    let mut rets: Vec<&str> = STATUS.iter().map(|&(_, r)| r).collect();
    rets.push("fp_fx");
    for op in OPS {
        for &arch in arches(op) {
            for one in [false, true] {
                let mut words: Vec<u32> = STATUS.iter().map(|&(b, _)| mtfsb(b, one)).collect();
                if one {
                    words.push(mtfsb(FX, false));
                }
                words.push(op.word);
                let lifted = lift(arch, &words, &rets, &[]);
                for (i, &(b, reg)) in STATUS.iter().enumerate() {
                    let want = if op.zeroes.contains(&b) {
                        Some(0)
                    } else if fprf_of(op).contains(&b) {
                        None
                    } else if op.sets.contains(&b) {
                        (one && b != FR && b != FI).then_some(1)
                    } else {
                        Some(u128::from(one))
                    };
                    assert_eq!(
                        lifted.folded(i),
                        want,
                        "{arch:?} {} after mtfsb{} of each: {reg}",
                        op.name,
                        u8::from(one)
                    );
                }
                if one {
                    assert_eq!(
                        lifted.folded(STATUS.len()),
                        Some(0),
                        "{arch:?} {} raised FX with every exception set",
                        op.name
                    );
                }
            }
        }
    }
}

fn image_bit(val: &Val, bit: usize) -> u128 {
    match val {
        Val::Int(x) => x >> (31 - bit) & 1,
        other => panic!("mffs gave {other:?}"),
    }
}

/// `mtfsb0 <bit>; <op>; mffs f4` reads the bit the op sets: an opaque one
/// as its op yields it, an exact one from the operands.
#[test]
fn mffs_reads_the_status_the_op_sets() {
    let (inf, ninf) = (f64::INFINITY, f64::NEG_INFINITY);
    // (op, f2, f3, bit, exact value or None for the opaque op's)
    let cases: [(&str, f64, f64, usize, Option<u128>); 23] = [
        ("fadd", 1.0, 2.0, FI, None),
        ("fadd", 1.0, 2.0, FR, None),
        ("fadd", 1.0, 2.0, OX, None),
        ("fadd", 1.0, 2.0, UX, None),
        ("fadd", 1.0, 2.0, XX, None),
        ("fdiv", 1.0, 3.0, FI, None),
        ("fmadd", 1.0, 3.0, FI, None),
        ("frsp", 1.0, 0.1, FI, None),
        ("fctiw", 1.0, 1.5, FI, None),
        ("fcfid", 1.0, 3.0, FI, None),
        ("fadd", inf, ninf, VXISI, Some(1)),
        ("fmul", inf, 0.0, VXIMZ, Some(1)),
        ("fdiv", inf, ninf, VXIDI, Some(1)),
        ("fdiv", 1.0, 0.0, ZX, Some(1)),
        ("fdiv", 0.0, 0.0, ZX, Some(0)),
        ("fsqrt", 1.0, -1.0, VXSQRT, Some(1)),
        ("frsqrte", 1.0, 0.0, ZX, Some(1)),
        ("fadd", 5e-324, 0.0, C, Some(1)),
        ("fdiv", 1.0, 0.0, FU, Some(1)),
        ("fadd", f64::NAN, 1.0, VXSNAN, Some(0)),
        ("fres", 1.0, 3.0, XX, Some(0)),
        ("fcmpu", SNAN, 1.0, VXSNAN, Some(1)),
        ("fcmpo", f64::NAN, 1.0, VXVC, Some(1)),
    ];
    for (name, a, b, bit, exact) in cases {
        let op = OPS.iter().find(|o| o.name == name).expect("an op");
        for &arch in arches(*op) {
            let lifted = lift(
                arch,
                &[mtfsb(bit, false), op.word, MFFS],
                &["f4"],
                &["f2", "f3", "f4"],
            );
            let vals = [a.to_bits(), b.to_bits(), 1.0f64.to_bits()].map(u128::from);
            for x in [0, 1] {
                let got = image_bit(&lifted.run_with(&vals, 0, &|_| Some(x))[0], bit);
                assert_eq!(
                    got,
                    exact.unwrap_or(x),
                    "{arch:?} {name} {a} {b}, opaque ops {x}: FPSCR bit {bit}"
                );
            }
        }
    }
}

const SNAN: f64 = f64::from_bits(0x7ff0_0000_0000_0001);

/// Every operand the invalid-operation checks distinguish.
const SPECIALS: [f64; 12] = [
    0.0,
    -0.0,
    1.0,
    -1.5,
    f64::MAX,
    -f64::MAX,
    5e-324,
    f64::INFINITY,
    f64::NEG_INFINITY,
    f64::NAN,
    -f64::NAN,
    SNAN,
];

fn is_zero(x: f64) -> bool {
    x == 0.0
}

/// A signalling NaN: its quiet bit, fraction bit 51, clear.
fn snan(x: f64) -> bool {
    x.is_nan() && x.to_bits() & 1 << 51 == 0
}

/// The exact bits, from initial clear status and the opaque ops yielding 0.
const EXACT: [(usize, &str); 7] = [
    (ZX, "fp_zx"),
    (VXSNAN, "fp_vxsnan"),
    (VXISI, "fp_vxisi"),
    (VXIDI, "fp_vxidi"),
    (VXZDZ, "fp_vxzdz"),
    (VXIMZ, "fp_vximz"),
    (VXSQRT, "fp_vxsqrt"),
];

fn exact_bits(lifted: &Lifted, vals: &[f64]) -> Vec<(usize, bool)> {
    let vals: Vec<u128> = vals.iter().map(|x| u128::from(x.to_bits())).collect();
    let out = lifted.run_with(&vals, 0, &|_| Some(0));
    EXACT
        .iter()
        .zip(out)
        .map(|(&(b, _), v)| match v {
            Val::Int(x) => (b, x == 1),
            other => panic!("a status bit is {other:?}"),
        })
        .collect()
}

/// ZX, VXISI, VXIDI, VXZDZ, VXIMZ, VXSQRT and VXSNAN are the operands' own,
/// over every pair (and triple) of specials; VXSNAN is a signalling NaN's
/// alone.
#[test]
fn invalid_and_zero_divide_bits_follow_the_operands() {
    let rets: Vec<&str> = EXACT.iter().map(|&(_, r)| r).collect();
    let inf = f64::is_infinite;
    let opposite = |a: f64, b: f64| a.is_sign_negative() != b.is_sign_negative();
    for name in [
        "fadd", "fadds", "fsub", "fsubs", "fmul", "fmuls", "fdiv", "fdivs",
    ] {
        let op = OPS.iter().find(|o| o.name == name).expect("an op");
        for &arch in arches(*op) {
            let lifted = lift(arch, &[op.word], &rets, &["f2", "f3"]);
            for a in SPECIALS {
                for b in SPECIALS {
                    for (bit, got) in exact_bits(&lifted, &[a, b]) {
                        let want = match (&name[..4], bit) {
                            (_, VXSNAN) => snan(a) || snan(b),
                            ("fadd", VXISI) => inf(a) && inf(b) && opposite(a, b),
                            ("fsub", VXISI) => inf(a) && inf(b) && !opposite(a, b),
                            ("fmul", VXIMZ) => inf(a) && is_zero(b) || is_zero(a) && inf(b),
                            ("fdiv", ZX) => is_zero(b) && a.is_finite() && !is_zero(a),
                            ("fdiv", VXIDI) => inf(a) && inf(b),
                            ("fdiv", VXZDZ) => is_zero(a) && is_zero(b),
                            _ => false,
                        };
                        assert_eq!(got, want, "{arch:?} {name} {a} {b}: FPSCR bit {bit}");
                    }
                }
            }
        }
    }
    for name in [
        "fsqrt", "fsqrts", "frsqrte", "frsqrtes", "fres", "fre", "frin", "friz",
    ] {
        let op = OPS.iter().find(|o| o.name == name).expect("an op");
        let ri = name.starts_with("fri");
        for &arch in arches(*op) {
            let lifted = lift(arch, &[op.word], &rets, &["f3"]);
            for b in SPECIALS {
                for (bit, got) in exact_bits(&lifted, &[b]) {
                    let want = match bit {
                        VXSNAN => snan(b),
                        ZX => !name.starts_with("fsqrt") && !ri && is_zero(b),
                        VXSQRT => name.contains("sqrt") && b < 0.0,
                        _ => false,
                    };
                    assert_eq!(got, want, "{arch:?} {name} {b}: FPSCR bit {bit}");
                }
            }
        }
    }
    for op in OPS.iter().filter(|o| o.sets == FMA) {
        let sub = op.name.contains("sub");
        for &arch in arches(*op) {
            let lifted = lift(arch, &[op.word], &rets, &["f2", "f3", "f4"]);
            for a in SPECIALS {
                for c in SPECIALS {
                    for b in SPECIALS {
                        let nan = a.is_nan() || b.is_nan() || c.is_nan();
                        let imz = inf(a) && is_zero(c) || is_zero(a) && inf(c);
                        let prod_inf = !nan && !imz && (inf(a) || inf(c));
                        let prod_neg = opposite(a, c);
                        let addend_neg = b.is_sign_negative() != sub;
                        let isi = prod_inf && inf(b) && prod_neg != addend_neg;
                        // f2, f3, f4 are A, B, C.
                        for (bit, got) in exact_bits(&lifted, &[a, b, c]) {
                            let want = match bit {
                                VXSNAN => snan(a) || snan(b) || snan(c),
                                VXIMZ => imz,
                                VXISI => isi,
                                _ => false,
                            };
                            assert_eq!(
                                got, want,
                                "{arch:?} {} {a} {c} {b}: FPSCR bit {bit}",
                                op.name
                            );
                        }
                    }
                }
            }
        }
    }
}

/// The class FPRF codes for `x`, a result in a format whose smallest normal
/// is `tiny`: C, FL, FG, FE, FU (Book I, Figure 4.6).
fn class(x: f64, tiny: f64) -> [bool; 5] {
    let neg = x.is_sign_negative();
    if x.is_nan() {
        [true, false, false, false, true]
    } else if x.is_infinite() {
        [false, neg, !neg, false, true]
    } else if x == 0.0 {
        [neg, false, false, true, false]
    } else {
        [x.abs() < tiny, neg, !neg, false, false]
    }
}

fn as_f64(v: &Val) -> f64 {
    match *v {
        Val::Float(x) => x,
        Val::Int(x) => f64::from_bits(u64::try_from(x).expect("a double")),
        Val::Quad(_) => panic!("f1 is a quad"),
    }
}

/// FPRF is the class of the result in the instruction's precision, over
/// every pair of specials and denormals of either format.
#[test]
fn fprf_is_the_result_class() {
    let mut vals = SPECIALS.to_vec();
    vals.extend([1e-39, -1e-39, 1e-310, -1e-310, 3.0, f64::MIN_POSITIVE]);
    let rets = ["f1", "fp_c", "fp_cc0", "fp_cc1", "fp_cc2", "fp_cc3"];
    let single = 2f64.powi(-126);
    for op in OPS {
        if op.sets == TO_INT || op.sets == TO_INT_Z || op.name.starts_with("fcmp") {
            continue;
        }
        if op.name == "xsadddp" || op.sets == FROM_INT {
            continue;
        }
        let tiny = if op.name.ends_with('s') || op.name == "frsp" {
            single
        } else {
            f64::MIN_POSITIVE
        };
        let arch = arches(op)[0];
        let lifted = lift(arch, &[op.word], &rets, &["f2", "f3", "f4"]);
        for &a in &vals {
            for &b in &vals {
                let bits = [a, b, 1.5].map(|x| u128::from(x.to_bits()));
                let out = lifted.run_with(&bits, 0, &|_| Some(0));
                let r = as_f64(&out[0]);
                let got = [1, 2, 3, 4, 5].map(|i| match out[i] {
                    Val::Int(x) => x == 1,
                    other => panic!("an FPRF bit is {other:?}"),
                });
                assert_eq!(
                    got,
                    class(r, tiny),
                    "{arch:?} {} {a} {b} = {r}: FPRF",
                    op.name
                );
            }
        }
    }
}

/// FPRF after an integer conversion to a float is the class of the result.
#[test]
fn fprf_of_a_conversion_from_an_integer() {
    let rets = ["f1", "fp_c", "fp_cc0", "fp_cc1", "fp_cc2", "fp_cc3"];
    for op in OPS.iter().filter(|o| o.sets == FROM_INT) {
        let tiny = if op.name.ends_with('s') {
            2f64.powi(-126)
        } else {
            f64::MIN_POSITIVE
        };
        let arch = arches(*op)[0];
        // C set before, so a stale C shows.
        let lifted = lift(arch, &[op.word], &rets, &["f3", "fp_c"]);
        for x in [0u64, 1, 5, u64::MAX, 1 << 63, (1 << 63) - 1, (1 << 53) + 1] {
            let out = lifted.run_with(&[u128::from(x), 1], 0, &|_| Some(0));
            let r = as_f64(&out[0]);
            let got = [1, 2, 3, 4, 5].map(|i| matches!(out[i], Val::Int(1)));
            assert_eq!(
                got,
                class(r, tiny),
                "{arch:?} {} {x:#x} = {r}: FPRF",
                op.name
            );
        }
    }
}

/// `fcmpu` raises VXSNAN for a signalling NaN; `fcmpo` also raises VXVC for
/// any NaN, except a signalling one under VE=1. FX rises with either, and VX
/// is their summary.
#[test]
fn compares_raise_the_invalid_bits() {
    let rets = ["fp_vxsnan", "fp_vxvc", "fp_fx", "fp_vx"];
    for (name, ordered) in [("fcmpu", false), ("fcmpo", true)] {
        let op = OPS.iter().find(|o| o.name == name).expect("an op");
        for &arch in arches(*op) {
            let lifted = lift(arch, &[op.word], &rets, &["f2", "f3", "fp_ve"]);
            for a in SPECIALS {
                for b in SPECIALS {
                    for ve in [false, true] {
                        let vals = [a.to_bits(), b.to_bits(), u64::from(ve)].map(u128::from);
                        let out = lifted.run_with(&vals, 0, &|_| None);
                        let got = [0, 1, 2, 3].map(|i| matches!(out[i], Val::Int(1)));
                        let sn = snan(a) || snan(b);
                        let vc = ordered && (a.is_nan() || b.is_nan()) && !(sn && ve);
                        let want = [sn, vc, sn || vc, sn || vc];
                        assert_eq!(got, want, "{arch:?} {name} {a} {b} VE={ve}");
                    }
                }
            }
        }
    }
}
