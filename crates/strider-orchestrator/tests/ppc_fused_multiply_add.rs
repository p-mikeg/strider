//! PowerPC fused multiply-add (Power ISA 3.0B, Book I 4.6.5.2): `fmadd`,
//! `fmsub`, `fnmadd` and `fnmsub`, with their single and record forms,
//! compute A*C ± B exactly and round it once, to single precision for the
//! `s` forms; the `n` forms negate the rounded result. Each is lifted as
//! `<form> f1, f2, f3, f4; blr` with f2, f3 and f4 (A, C and B) initial and
//! f1 returned, and checked against the host's fused multiply-add.

mod common;

use common::Arch;
use common::cone::{Lifted, Val, lift_regs};
use strider_ir_test_utils::Rng;
use strider_orchestrator::opt::AssumptionOptions;

const ARCHES: [Arch; 4] = [Arch::Ppc32be, Arch::Ppc32le, Arch::Ppc64be, Arch::Ppc64le];
const BLR: u32 = 0x4e80_0020;

#[derive(Clone, Copy)]
struct Form {
    name: &'static str,
    word: u32,
    sub: bool,
    neg: bool,
    single: bool,
}

const fn form(name: &'static str, word: u32, sub: bool, neg: bool, single: bool) -> Form {
    Form {
        name,
        word,
        sub,
        neg,
        single,
    }
}

/// Every form as `<form> 1, 2, 3, 4` (llvm-mc).
const FORMS: [Form; 16] = [
    form("fmadd", 0xfc22_20fa, false, false, false),
    form("fmadd.", 0xfc22_20fb, false, false, false),
    form("fmadds", 0xec22_20fa, false, false, true),
    form("fmadds.", 0xec22_20fb, false, false, true),
    form("fmsub", 0xfc22_20f8, true, false, false),
    form("fmsub.", 0xfc22_20f9, true, false, false),
    form("fmsubs", 0xec22_20f8, true, false, true),
    form("fmsubs.", 0xec22_20f9, true, false, true),
    form("fnmadd", 0xfc22_20fe, false, true, false),
    form("fnmadd.", 0xfc22_20ff, false, true, false),
    form("fnmadds", 0xec22_20fe, false, true, true),
    form("fnmadds.", 0xec22_20ff, false, true, true),
    form("fnmsub", 0xfc22_20fc, true, true, false),
    form("fnmsub.", 0xfc22_20fd, true, true, false),
    form("fnmsubs", 0xec22_20fc, true, true, true),
    form("fnmsubs.", 0xec22_20fd, true, true, true),
];

fn lift(arch: Arch, f: Form, claims: &AssumptionOptions) -> Lifted {
    lift_regs(arch, &[f.word, BLR], &["f1"], &["f2", "f3", "f4"], claims)
}

/// The ISA result: A*C ± B rounded once, then negated for the `n` forms.
fn want(f: Form, a: f64, c: f64, b: f64) -> f64 {
    let b = if f.sub { -b } else { b };
    let r = if f.single {
        let (a, c, b) = (a as f32, c as f32, b as f32);
        f64::from(a.mul_add(c, b))
    } else {
        a.mul_add(c, b)
    };
    if f.neg { -r } else { r }
}

fn got(lifted: &Lifted, a: f64, c: f64, b: f64, fill: u128) -> f64 {
    let vals = [a, c, b].map(|x| u128::from(x.to_bits()));
    match lifted.run(&vals, fill)[..] {
        [Val::Int(x)] => f64::from_bits(u64::try_from(x).expect("f1 is 64 bits")),
        [Val::Float(x)] => x,
        ref other => panic!("f1 is {other:?}"),
    }
}

/// Counts the cases where the lift disagrees with the ISA, naming the first.
fn check(arch: Arch, f: Form, lifted: &Lifted, cases: &[[f64; 3]], fill: u128) -> usize {
    let mut bad = 0;
    for &[a, c, b] in cases {
        let (want, got) = (want(f, a, c, b), got(lifted, a, c, b, fill));
        let same = if want.is_nan() {
            got.is_nan()
        } else {
            got.to_bits() == want.to_bits()
        };
        if !same {
            if bad == 0 {
                eprintln!(
                    "{arch:?} {}: A={a:e} ({:#x}) C={c:e} ({:#x}) B={b:e} ({:#x}): \
                     got {got:e} ({:#x}), want {want:e} ({:#x})",
                    f.name,
                    a.to_bits(),
                    c.to_bits(),
                    b.to_bits(),
                    got.to_bits(),
                    want.to_bits()
                );
            }
            bad += 1;
        }
    }
    bad
}

/// Signed zeros, infinities, quiet and signalling NaNs, the denormal and
/// normal extremes, and plain values.
const SPECIALS: [f64; 14] = [
    0.0,
    -0.0,
    1.0,
    -1.5,
    3.0,
    -0.7,
    5e-324,
    -2.225_073_858_507_201e-308,
    f64::MIN_POSITIVE,
    f64::MAX,
    -2_147_483_648.0,
    f64::INFINITY,
    f64::NEG_INFINITY,
    f64::NAN,
];

/// A signalling NaN.
const SNAN: u64 = 0x7ff0_0000_0000_0001;

fn special_cases() -> Vec<[f64; 3]> {
    let mut out = Vec::new();
    for a in SPECIALS {
        for c in SPECIALS {
            for b in SPECIALS {
                out.push([a, c, b]);
            }
        }
    }
    let snan = f64::from_bits(SNAN);
    out.extend([[snan, 1.0, 1.0], [1.0, snan, 1.0], [1.0, 1.0, snan]]);
    out
}

/// Products one rounding away from the answer: an exact midpoint nudged by
/// a tiny addend, an infinity added to a finite product past `f64::MAX`,
/// a denormal product's sign, and cancellation.
fn crafted_cases() -> Vec<[f64; 3]> {
    let x = f64::from_bits;
    vec![
        // 3 * 0x1.5555555555556p-2 = 1 + 2^-53, the midpoint below 1+ulp.
        [3.0, x(0x3fd5_5555_5555_5556), 2f64.powi(-200)],
        [3.0, x(0x3fd5_5555_5555_5556), -(2f64.powi(-200))],
        [x(0xc1e0_0000_0000_0000), f64::MAX, f64::INFINITY],
        [x(0xbfe6_6666_6666_6666), -2.5, 5e-324],
        [-2.4e-32, 2.2e-308, 0.0],
        [x(0x3ff0_0000_1000_0000), -f64::MAX, -f64::MAX],
        [f64::MAX, 2.0, -f64::MAX],
        [f64::MAX, 2.0, f64::NEG_INFINITY],
        [0.1, 10.0, -1.0],
        [1.0 + f64::EPSILON, 1.0 - f64::EPSILON / 2.0, -1.0],
    ]
}

/// A random finite double.
fn double(rng: &mut Rng) -> f64 {
    loop {
        let x = f64::from_bits(rng.next_u64());
        if x.is_finite() {
            return x;
        }
    }
}

/// Random triples, half with B near -A*C, and products at a double's exact
/// midpoint with a tiny B on either side.
fn random_cases(n: usize, seed: u64) -> Vec<[f64; 3]> {
    let mut rng = Rng(seed);
    let mut out = Vec::new();
    for i in 0..n {
        let (a, c) = (double(&mut rng), double(&mut rng));
        let b = if i % 2 == 0 {
            double(&mut rng)
        } else {
            let nudge = [0.0, f64::EPSILON, -f64::EPSILON, 2f64.powi(-30)][rng.below(4)];
            -(a * c) * (1.0 + nudge)
        };
        if b.is_finite() {
            out.push([a, c, b]);
        }
    }
    for _ in 0..n / 4 {
        let m = rng.next_u64() >> 12 | 1 << 52;
        let num = 2 * m + 1;
        let Some(k) = [3u64, 5, 7, 9, 11, 13]
            .into_iter()
            .find(|k| num.is_multiple_of(*k) && num / k < 1 << 53)
        else {
            continue;
        };
        let c = (num / k) as f64 * 2f64.powi(-53);
        for b in [2f64.powi(-200), -(2f64.powi(-200)), 2f64.powi(-120)] {
            out.push([k as f64, c, b]);
        }
    }
    out
}

/// A random single with a moderate exponent, as a double.
fn single(rng: &mut Rng) -> f64 {
    let bits = (rng.next_u64() & 0x7f_ffff) as u32 | (100 + rng.below(55) as u32) << 23;
    let x = f32::from_bits(bits | if rng.chance(50) { 1 << 31 } else { 0 });
    f64::from(x)
}

/// Single triples whose B nearly cancels A*C, where a double rounding of
/// the sum shows.
fn single_cases(n: usize, seed: u64) -> Vec<[f64; 3]> {
    let mut rng = Rng(seed);
    let mut out = Vec::new();
    for _ in 0..n {
        let (a, c) = (single(&mut rng), single(&mut rng));
        let k = [24, 25, 26, 40, 60][rng.below(5)];
        let s = [1.0, -1.0, 3.0][rng.below(3)];
        let b = f64::from((-(a * c) * (1.0 + s * 2f64.powi(-k))) as f32);
        out.push([a, c, b]);
    }
    // A*C an exact single midpoint, which a tiny B moves off: the sum rounded
    // to a double first lands back on the midpoint.
    for _ in 0..n / 4 {
        let (m1, m2) = (
            rng.below(1 << 12) | 1 << 12 | 1,
            rng.below(1 << 12) | 1 << 12 | 1,
        );
        if (m1 * m2) >> 24 != 1 {
            continue;
        }
        let (a, c) = (m1 as f64 * 2f64.powi(-12), m2 as f64 * 2f64.powi(-12));
        for b in [2f64.powi(-60), -(2f64.powi(-60))] {
            out.push([a, c, b]);
        }
    }
    out.push([1.0 + 2f64.powi(-12), 1.0 + 2f64.powi(-12), 2f64.powi(-60)]);
    out.push([f64::from_bits(0x4560_f861_a000_0000), -1.5, 1.5]);
    for a in [0.0, -0.0, 1.0, -1.5, f64::INFINITY, f64::NAN] {
        for b in [0.0, -0.0, 2.5, f64::NEG_INFINITY] {
            out.push([a, 3.0, b]);
        }
    }
    out
}

#[test]
fn double_forms_round_once() {
    let claims = AssumptionOptions::default();
    let cases: Vec<_> = crafted_cases()
        .into_iter()
        .chain(special_cases())
        .chain(random_cases(2000, 0x5eed_f3a0))
        .collect();
    let mut bad = 0;
    for arch in ARCHES {
        for f in FORMS.into_iter().filter(|f| !f.single) {
            bad += check(arch, f, &lift(arch, f, &claims), &cases, 0);
        }
    }
    assert_eq!(bad, 0, "mismatches over {} cases", cases.len());
}

#[test]
fn single_forms_round_once_to_single() {
    let claims = AssumptionOptions::default();
    let cases = single_cases(2000, 0x5eed_f3a1);
    let mut bad = 0;
    for arch in ARCHES {
        for f in FORMS.into_iter().filter(|f| f.single) {
            bad += check(arch, f, &lift(arch, f, &claims), &cases, 0);
        }
    }
    assert_eq!(bad, 0, "mismatches over {} cases", cases.len());
}

/// The result reads no register but A, B and C: FPSCR.RN set to 3 by the
/// all-ones fill changes nothing, with no claim to take it as 0.
#[test]
fn result_reads_only_its_operands() {
    let claims = AssumptionOptions::none();
    let cases: Vec<_> = crafted_cases()
        .into_iter()
        .chain(random_cases(200, 0x5eed_f3a2))
        .collect();
    let singles = single_cases(200, 0x5eed_f3a3);
    let mut bad = 0;
    for arch in [Arch::Ppc64le, Arch::Ppc32be] {
        for f in FORMS {
            let lifted = lift(arch, f, &claims);
            let cases = if f.single { &singles } else { &cases };
            bad += check(arch, f, &lifted, cases, 0);
            bad += check(arch, f, &lifted, cases, u128::MAX);
        }
    }
    assert_eq!(bad, 0);
}
