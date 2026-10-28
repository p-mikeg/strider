//! The float reference oracle's own rules, which no conversion test reaches:
//! the shift-past-the-mantissa branch of `round::Fmt::round`, and the 128-bit
//! arithmetic `quad` builds on it.

#![allow(dead_code)]

#[path = "common/quad.rs"]
mod quad;
#[path = "common/round.rs"]
mod round;

use quad::QUAD;
use round::{DOUBLE, Mode};

/// `DOUBLE`'s smallest denormal is `m * 2^e` with `e` this far below the
/// value's own exponent, so `round` shifts `m` out by exactly 128.
const SHIFT_128: i32 = -1202;
/// One more, so the shift is 129.
const SHIFT_129: i32 = -1203;

const HALF_ULP: u128 = 1 << 127;

#[test]
fn a_shift_of_128_keeps_the_remainder() {
    // Above the half point: nearest-even rounds up to the smallest denormal.
    assert_eq!(DOUBLE.round(false, HALF_ULP + 1, SHIFT_128, Mode::Even), 1);
    assert_eq!(DOUBLE.round(false, u128::MAX, SHIFT_128, Mode::Even), 1);
    // Exactly half, and the kept digit is even: down.
    assert_eq!(DOUBLE.round(false, HALF_ULP, SHIFT_128, Mode::Even), 0);
    // Below half: down.
    assert_eq!(DOUBLE.round(false, HALF_ULP - 1, SHIFT_128, Mode::Even), 0);
}

#[test]
fn a_shift_of_128_carries_the_sign() {
    let neg = 1u128 << 63;
    assert_eq!(
        DOUBLE.round(true, HALF_ULP + 1, SHIFT_128, Mode::Even),
        neg | 1
    );
    assert_eq!(DOUBLE.round(true, HALF_ULP, SHIFT_128, Mode::Even), neg);
}

#[test]
fn a_shift_past_the_mantissa_is_still_inexact() {
    // Every mode but nearest decides on the inexact flag alone, which a
    // shift of 128 or more must still report.
    for (sh, m) in [
        (SHIFT_128, 1u128),
        (SHIFT_128, HALF_ULP),
        (SHIFT_129, 1u128),
        (SHIFT_129, u128::MAX),
    ] {
        assert_eq!(DOUBLE.round(false, m, sh, Mode::Up), 1, "{m:#x} {sh}");
        assert_eq!(DOUBLE.round(true, m, sh, Mode::Up), 1 << 63, "{m:#x} {sh}");
        assert_eq!(DOUBLE.round(false, m, sh, Mode::Zero), 0, "{m:#x} {sh}");
        assert_eq!(DOUBLE.round(false, m, sh, Mode::Odd), 1, "{m:#x} {sh}");
    }
}

#[test]
fn a_shift_of_129_cannot_reach_the_half_point() {
    // `m` is at most `2^128 - 1`, so a shift of 129 leaves less than half.
    assert_eq!(DOUBLE.round(false, u128::MAX, SHIFT_129, Mode::Even), 0);
    assert_eq!(DOUBLE.round(false, HALF_ULP, SHIFT_129, Mode::Even), 0);
}

/// Bit patterns spanning `QUAD`'s denormals, its normal range and its
/// extremes, as a seeded walk so the table is short.
fn quad_samples() -> Vec<u128> {
    let mut out = vec![
        0,
        1,
        0x7fff_u128 << 112,
        (0x7fff_u128 << 112) | 1,
        (1u128 << 112) - 1,
        1u128 << 112,
        (0x3fff_u128 << 112) | 0x1234_5678_9abc_def0,
        (0x4001_u128 << 112) | 1,
        (0x7ffe_u128 << 112) | ((1u128 << 112) - 1),
    ];
    let mut s = 0x2545_f491_4f6c_dd1d_u64;
    for _ in 0..24 {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        let hi = u128::from(s) << 64;
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        out.push(hi | u128::from(s));
    }
    let neg: Vec<u128> = out.iter().map(|b| b ^ (1u128 << 127)).collect();
    out.extend(neg);
    out
}

/// A digest of `quad`'s arithmetic over those samples. The oracle is the
/// reference the F128 semantics tests are held to, so its answers may not
/// drift when its rounder is shared with `round`.
#[test]
fn quad_arithmetic_digest() {
    // FNV-1a, not `DefaultHasher`, whose output is free to change between
    // compiler releases.
    let mut h = 0xcbf2_9ce4_8422_2325_u64;
    let mut eat = |v: u128| {
        for b in v.to_le_bytes() {
            h = (h ^ u64::from(b)).wrapping_mul(0x100_0000_01b3);
        }
    };
    let s = quad_samples();
    for a in &s {
        eat(quad::neg(QUAD, *a));
        eat(quad::from_int(QUAD, *a as i128));
        eat(quad::convert(QUAD, quad::DOUBLE, *a));
        eat(quad::convert(quad::DOUBLE, QUAD, *a & u128::from(u64::MAX)));
        for b in &s {
            eat(quad::add(QUAD, *a, *b));
            eat(quad::mul(QUAD, *a, *b));
            eat(quad::div(QUAD, *a, *b));
            eat(match quad::cmp(QUAD, *a, *b) {
                None => 3,
                Some(std::cmp::Ordering::Less) => 0,
                Some(std::cmp::Ordering::Equal) => 1,
                Some(std::cmp::Ordering::Greater) => 2,
            });
        }
    }
    assert_eq!(h, 2_093_867_834_129_376_716, "quad arithmetic digest");
}
