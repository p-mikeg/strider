//! MIPS float-to-integer conversions (MIPS32/MIPS64 Vol. II, CVT, ROUND,
//! TRUNC, FLOOR and CEIL .W/.L): `round` rounds to nearest even, `trunc`
//! toward zero, `floor` toward -inf, `ceil` toward +inf, and `cvt` by
//! FCSR.RM (bits 0-1: 0 nearest even, 1 toward zero, 2 toward +inf, 3
//! toward -inf). A NaN, an infinity or a result past the destination's range
//! gives the signed maximum, the default under FCSR.NAN2008 = 0.
//! `fp_rounding_mode_is_nearest` takes RM to be 0; without it `cvt` reads it.
//!
//! Each function is `lui $1, 1; {lwc1,ldc1} $f12, <lit>($1); [ori $8, $0,
//! rm; ctc1 $8, $31;] <conv> $f0, $f12; {mfc1,dmfc1} $2, $f0; jr $ra; nop`,
//! the literal 48 bytes in, at a 64-byte stride; every encoding is
//! llvm-mc's (`mips32r2`, `mips64r2`).

use crate::common;

use crate::common::Arch;
use strider_orchestrator::opt::AssumptionOptions;

const BASE: u64 = 0x10000;
const STRIDE: usize = 64;
const LIT: usize = 48;

#[derive(Clone, Copy)]
struct Conv {
    asm: &'static str,
    word: u32,
    /// FCSR.RM's encoding of the rounding, or `None` for `cvt`, which reads
    /// it.
    rm: Option<u32>,
    double: bool,
}

const fn conv(asm: &'static str, word: u32, rm: Option<u32>, double: bool) -> Conv {
    Conv {
        asm,
        word,
        rm,
        double,
    }
}

const WORDS: [Conv; 10] = [
    conv("cvt.w.s", 0x4600_6024, None, false),
    conv("cvt.w.d", 0x4620_6024, None, true),
    conv("round.w.s", 0x4600_600c, Some(0), false),
    conv("round.w.d", 0x4620_600c, Some(0), true),
    conv("trunc.w.s", 0x4600_600d, Some(1), false),
    conv("trunc.w.d", 0x4620_600d, Some(1), true),
    conv("ceil.w.s", 0x4600_600e, Some(2), false),
    conv("ceil.w.d", 0x4620_600e, Some(2), true),
    conv("floor.w.s", 0x4600_600f, Some(3), false),
    conv("floor.w.d", 0x4620_600f, Some(3), true),
];

const LONGS: [Conv; 10] = [
    conv("cvt.l.s", 0x4600_6025, None, false),
    conv("cvt.l.d", 0x4620_6025, None, true),
    conv("round.l.s", 0x4600_6008, Some(0), false),
    conv("round.l.d", 0x4620_6008, Some(0), true),
    conv("trunc.l.s", 0x4600_6009, Some(1), false),
    conv("trunc.l.d", 0x4620_6009, Some(1), true),
    conv("ceil.l.s", 0x4600_600a, Some(2), false),
    conv("ceil.l.d", 0x4620_600a, Some(2), true),
    conv("floor.l.s", 0x4600_600b, Some(3), false),
    conv("floor.l.d", 0x4620_600b, Some(3), true),
];

/// Ties each way and sign, each destination's bounds and the doubles either
/// side of them, and values neither holds.
const VALUES: [f64; 32] = [
    0.0,
    -0.0,
    0.5,
    -0.5,
    1.5,
    -1.5,
    2.5,
    -2.5,
    1.2,
    -1.2,
    2.7,
    -2.7,
    5e-324,
    -5e-324,
    2_147_483_647.5,
    2_147_483_648.0,
    -2_147_483_648.5,
    -2_147_483_649.0,
    3e9,
    -3e9,
    9_223_372_036_854_774_784.0,
    9_223_372_036_854_775_808.0,
    -9_223_372_036_854_775_808.0,
    -9_223_372_036_854_777_856.0,
    1e19,
    -1e19,
    1e300,
    -1e300,
    f64::NAN,
    -f64::NAN,
    f64::INFINITY,
    f64::NEG_INFINITY,
];

fn input(conv: Conv, x: f64) -> (f64, u64) {
    if conv.double {
        (x, x.to_bits())
    } else {
        let s = x as f32;
        (f64::from(s), u64::from(s.to_bits()))
    }
}

/// The ISA's answer for a `bits`-wide destination under the legacy default.
fn expected(conv: Conv, rm: u32, x: f64, bits: u32) -> u64 {
    let max = (1i128 << (bits - 1)) - 1;
    let r = match conv.rm.unwrap_or(rm) {
        0 => x.round_ties_even(),
        1 => x.trunc(),
        2 => x.ceil(),
        _ => x.floor(),
    };
    let i = if r.is_finite() && (-max - 1..=max).contains(&(r as i128)) {
        r as i128
    } else {
        max
    };
    (i as u64) & (u64::MAX >> (64 - bits))
}

/// One function per value, `rm` written to FCSR first when given; `mover`
/// reads the result into $2.
fn image(arch: Arch, conv: Conv, rm: Option<u32>, mover: u32, lits: &[u64]) -> (Vec<u8>, Vec<u64>) {
    let big = matches!(arch, Arch::Mips32be | Arch::Mips64be);
    let put = |b: &mut [u8], w: &[u8]| b.copy_from_slice(w);
    let mut bytes = vec![0; lits.len() * STRIDE];
    let mut entries = Vec::new();
    for (i, &lit) in lits.iter().enumerate() {
        let at = i * STRIDE;
        let off = u32::try_from(at + LIT).unwrap();
        let load = if conv.double {
            0xd42c_0000
        } else {
            0xc42c_0000
        };
        let mut words = vec![0x3c01_0001, load | off];
        if let Some(rm) = rm {
            words.extend([0x3408_0000 | rm, 0x44c8_f800]);
        }
        words.extend([conv.word, mover, 0x03e0_0008, 0]);
        for (j, w) in words.iter().enumerate() {
            let w = if big {
                w.to_be_bytes()
            } else {
                w.to_le_bytes()
            };
            put(&mut bytes[at + 4 * j..at + 4 * j + 4], &w);
        }
        let lit = if conv.double {
            if big {
                lit.to_be_bytes().to_vec()
            } else {
                lit.to_le_bytes().to_vec()
            }
        } else {
            let w = lit as u32;
            if big {
                w.to_be_bytes().to_vec()
            } else {
                w.to_le_bytes().to_vec()
            }
        };
        put(&mut bytes[at + LIT..at + LIT + lit.len()], &lit);
        entries.push(BASE + at as u64);
    }
    (bytes, entries)
}

#[allow(clippy::too_many_arguments)]
fn check(
    arch: Arch,
    conv: Conv,
    rm: Option<u32>,
    mover: u32,
    bits: u32,
    init: u128,
    claims: &AssumptionOptions,
    want_rm: u32,
) {
    let inputs: Vec<(f64, u64)> = VALUES.iter().map(|&x| input(conv, x)).collect();
    let lits: Vec<u64> = inputs.iter().map(|&(_, b)| b).collect();
    let (bytes, entries) = image(arch, conv, rm, mover, &lits);
    let got = common::cone::returned_at_under(arch, bytes, BASE, &entries, init, claims);
    for (&(x, _), got) in inputs.iter().zip(got) {
        let got = got & (u64::MAX >> (64 - bits));
        assert_eq!(
            got,
            expected(conv, want_rm, x, bits),
            "{arch:?} {} rm {rm:?} init {init:#x} {x:e}",
            conv.asm
        );
    }
}

// mfc1 $2, $f0
const MFC1: u32 = 0x4402_0000;
// dmfc1 $2, $f0
const DMFC1: u32 = 0x4422_0000;

#[test]
fn word_conversions_round_and_saturate() {
    let none = AssumptionOptions::none();
    for arch in [Arch::Mips32be, Arch::Mips32le, Arch::Mips64be] {
        for conv in WORDS {
            for rm in 0..4 {
                check(arch, conv, Some(rm), MFC1, 32, 0, &none, rm);
            }
            // Every initial register reads `init`: 0 gives RM 0, all ones 3.
            check(arch, conv, None, MFC1, 32, 0, &none, 0);
            check(arch, conv, None, MFC1, 32, u128::MAX, &none, 3);
        }
    }
}

#[test]
fn doubleword_conversions_round_and_saturate() {
    let none = AssumptionOptions::none();
    for arch in [Arch::Mips64be, Arch::Mips64le] {
        for conv in LONGS {
            for rm in 0..4 {
                check(arch, conv, Some(rm), DMFC1, 64, 0, &none, rm);
            }
            check(arch, conv, None, DMFC1, 64, 0, &none, 0);
            check(arch, conv, None, DMFC1, 64, u128::MAX, &none, 3);
        }
    }
}

/// On a 32-bit FPU a doubleword result fills the even-odd pair, its high
/// word in the odd register: `mfc1 $2, $f0` and `mfc1 $2, $f1`.
#[test]
fn doubleword_conversions_fill_the_register_pair_on_mips32() {
    let none = AssumptionOptions::none();
    for conv in LONGS {
        for init in [0, u128::MAX] {
            let inputs: Vec<(f64, u64)> = VALUES.iter().map(|&x| input(conv, x)).collect();
            let lits: Vec<u64> = inputs.iter().map(|&(_, b)| b).collect();
            let halves: Vec<Vec<u64>> = [MFC1, 0x4402_0800]
                .into_iter()
                .map(|mover| {
                    let (bytes, entries) = image(Arch::Mips32be, conv, Some(1), mover, &lits);
                    common::cone::returned_at_under(
                        Arch::Mips32be,
                        bytes,
                        BASE,
                        &entries,
                        init,
                        &none,
                    )
                })
                .collect();
            for (k, &(x, _)) in inputs.iter().enumerate() {
                let got = (halves[0][k] & 0xffff_ffff) | halves[1][k] << 32;
                assert_eq!(
                    got,
                    expected(conv, 1, x, 64),
                    "{} init {init:#x} {x:e}",
                    conv.asm
                );
            }
        }
    }
}

/// Under the claim `cvt` reads RM as 0 whatever FCSR holds.
#[test]
fn the_claim_rounds_cvt_to_nearest_whatever_fcsr_holds() {
    let claims = AssumptionOptions::default();
    assert!(claims.fp_rounding_mode_is_nearest);
    for rm in [1, 2, 3] {
        check(Arch::Mips32be, WORDS[0], Some(rm), MFC1, 32, 0, &claims, 0);
        check(Arch::Mips32be, WORDS[1], Some(rm), MFC1, 32, 0, &claims, 0);
        check(Arch::Mips64be, LONGS[1], Some(rm), DMFC1, 64, 0, &claims, 0);
    }
}

/// `fcsrRoundingControl` is the last user op on every MIPS preset, so no
/// other id moved.
#[test]
fn the_rounding_mode_op_is_the_last_user_op() {
    for arch in [
        Arch::Mips32be,
        Arch::Mips32le,
        Arch::Mips64be,
        Arch::Mips64le,
    ] {
        let (strider, _) = common::strider_over_bytes(arch, vec![0; 4], BASE, None);
        let names = strider.user_op_names();
        assert_eq!(
            names.last().map(String::as_str),
            Some("fcsrRoundingControl"),
            "{arch:?}"
        );
    }
}
