//! ARM VFP float-to-integer conversions (Arm ARM DDI0487 F6.1.61 and
//! F6.1.75, `FPToFixed`): `vcvt` rounds toward zero, `vcvtr` by FPSCR.RMode
//! (bits 22-23: 0 nearest even, 1 toward +inf, 2 toward -inf, 3 toward
//! zero). A result past the 32-bit range saturates to its bound and a NaN
//! gives 0. `fp_rounding_mode_is_nearest` takes RMode to be 0; without it
//! `vcvtr` reads it.
//!
//! Each function is `vldr {d1,s2}, <lit>; [mov r1, #mode; vmsr fpscr, r1;]
//! <conv> s0, {d1,s2}; vmov r0, s0; bx lr`, the literal 48 bytes in, at a
//! 64-byte stride; every encoding is llvm-mc's (`armv8a`).

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
    /// Rounds by FPSCR.RMode; else toward zero.
    by_rmode: bool,
    unsigned: bool,
    double: bool,
}

const CONVS: [Conv; 8] = [
    Conv {
        asm: "vcvt.u32.f32 s0, s2",
        word: 0xeebc_0ac1,
        by_rmode: false,
        unsigned: true,
        double: false,
    },
    Conv {
        asm: "vcvt.s32.f32 s0, s2",
        word: 0xeebd_0ac1,
        by_rmode: false,
        unsigned: false,
        double: false,
    },
    Conv {
        asm: "vcvt.u32.f64 s0, d1",
        word: 0xeebc_0bc1,
        by_rmode: false,
        unsigned: true,
        double: true,
    },
    Conv {
        asm: "vcvt.s32.f64 s0, d1",
        word: 0xeebd_0bc1,
        by_rmode: false,
        unsigned: false,
        double: true,
    },
    Conv {
        asm: "vcvtr.u32.f32 s0, s2",
        word: 0xeebc_0a41,
        by_rmode: true,
        unsigned: true,
        double: false,
    },
    Conv {
        asm: "vcvtr.s32.f32 s0, s2",
        word: 0xeebd_0a41,
        by_rmode: true,
        unsigned: false,
        double: false,
    },
    Conv {
        asm: "vcvtr.u32.f64 s0, d1",
        word: 0xeebc_0b41,
        by_rmode: true,
        unsigned: true,
        double: true,
    },
    Conv {
        asm: "vcvtr.s32.f64 s0, d1",
        word: 0xeebd_0b41,
        by_rmode: true,
        unsigned: false,
        double: true,
    },
];

/// Ties each way and sign, the 32-bit bounds and the doubles either side
/// of them, and values no 32-bit integer holds.
const VALUES: [f64; 28] = [
    0.0,
    -0.0,
    0.5,
    -0.5,
    1.5,
    -1.5,
    2.5,
    -2.5,
    2.7,
    -2.7,
    5e-324,
    -5e-324,
    2_147_483_647.5,
    2_147_483_648.0,
    -2_147_483_648.5,
    -2_147_483_649.0,
    4_294_967_295.5,
    4_294_967_296.0,
    3e9,
    -3e9,
    1e19,
    -1e19,
    1e300,
    -1e300,
    f64::NAN,
    -f64::NAN,
    f64::INFINITY,
    f64::NEG_INFINITY,
];

/// The input `conv` reads for `x`: a double, or the single nearest it.
fn input(conv: Conv, x: f64) -> (f64, u64) {
    if conv.double {
        (x, x.to_bits())
    } else {
        let s = x as f32;
        (f64::from(s), u64::from(s.to_bits()))
    }
}

/// `FPToFixed(x, 0, unsigned, rmode)` for a 32-bit destination.
fn expected(conv: Conv, rmode: u32, x: f64) -> u64 {
    if x.is_nan() {
        return 0;
    }
    let r = match if conv.by_rmode { rmode } else { 3 } {
        0 => x.round_ties_even(),
        1 => x.ceil(),
        2 => x.floor(),
        _ => x.trunc(),
    };
    let (lo, hi): (i128, i128) = if conv.unsigned {
        (0, 0xffff_ffff)
    } else {
        (-(1 << 31), (1 << 31) - 1)
    };
    // An infinity casts to the i128 bound.
    ((r as i128).clamp(lo, hi) as u64) & 0xffff_ffff
}

/// One function per value, `rmode` written to FPSCR first when given.
fn image(conv: Conv, rmode: Option<u32>, lits: &[u64]) -> (Vec<u8>, Vec<u64>) {
    let mut bytes = vec![0; lits.len() * STRIDE];
    let mut entries = Vec::new();
    for (i, &lit) in lits.iter().enumerate() {
        let at = i * STRIDE;
        // vldr d1, [pc, #40] or vldr s2, [pc, #40]
        let mut words = vec![if conv.double {
            0xed9f_1b0a
        } else {
            0xed9f_1a0a
        }];
        if let Some(rmode) = rmode {
            // mov r1, #rmode << 22; vmsr fpscr, r1
            words.extend([0xe3a0_1500 | rmode, 0xeee1_1a10]);
        }
        // <conv>; vmov r0, s0; bx lr
        words.extend([conv.word, 0xee10_0a10, 0xe12f_ff1e]);
        for (j, w) in words.iter().enumerate() {
            bytes[at + 4 * j..at + 4 * j + 4].copy_from_slice(&w.to_le_bytes());
        }
        bytes[at + LIT..at + LIT + 8].copy_from_slice(&lit.to_le_bytes());
        entries.push(BASE + at as u64);
    }
    (bytes, entries)
}

fn run(conv: Conv, rmode: Option<u32>, init: u128, claims: &AssumptionOptions) -> Vec<(f64, u64)> {
    let inputs: Vec<(f64, u64)> = VALUES.iter().map(|&x| input(conv, x)).collect();
    let lits: Vec<u64> = inputs.iter().map(|&(_, b)| b).collect();
    let (bytes, entries) = image(conv, rmode, &lits);
    let got = common::cone::returned_at_under(Arch::Arm, bytes, BASE, &entries, init, claims);
    inputs.iter().map(|&(x, _)| x).zip(got).collect()
}

#[test]
fn conversions_saturate_and_vcvtr_rounds_by_fpscr_rmode() {
    let none = AssumptionOptions::none();
    for conv in CONVS {
        for rmode in 0..4 {
            for (x, got) in run(conv, Some(rmode), 0, &none) {
                assert_eq!(
                    got,
                    expected(conv, rmode, x),
                    "{} rmode {rmode} {x:e}",
                    conv.asm
                );
            }
        }
        // Every initial register reads `init`: 0 gives RMode 0, all ones 3.
        for (init, rmode) in [(0, 0), (u128::MAX, 3)] {
            for (x, got) in run(conv, None, init, &none) {
                assert_eq!(
                    got,
                    expected(conv, rmode, x),
                    "{} initial rmode {rmode} {x:e}",
                    conv.asm
                );
            }
        }
    }
}

/// Under the claim `vcvtr` reads RMode as 0 whatever FPSCR holds.
#[test]
fn the_claim_rounds_vcvtr_to_nearest_whatever_fpscr_holds() {
    let claims = AssumptionOptions::default();
    assert!(claims.fp_rounding_mode_is_nearest);
    for conv in CONVS {
        for rmode in [1, 2, 3] {
            for (x, got) in run(conv, Some(rmode), 0, &claims) {
                assert_eq!(
                    got,
                    expected(conv, 0, x),
                    "{} rmode {rmode} under the claim {x:e}",
                    conv.asm
                );
            }
        }
    }
}
