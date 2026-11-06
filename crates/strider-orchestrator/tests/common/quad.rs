//! 128-bit IEEE arithmetic on bit patterns, rounded to nearest even: the
//! cone's F128 values, which no host type holds. The formats, the decoder
//! and the rounder are [`super::round`]'s; what is here is the limb
//! arithmetic no host type provides. NaN payloads are unspecified.

use std::cmp::Ordering;

use super::round::{Fmt, Mode, Num};

pub(crate) const QUAD: Fmt = Fmt { exp: 15, man: 112 };

/// `bits` of `from` converted to `to`.
pub(crate) fn convert(from: Fmt, to: Fmt, bits: u128) -> u128 {
    to.convert(from, bits, Mode::Even)
        .unwrap_or_else(|| to.nan())
}

/// The signed integer `x` in `f`.
pub(crate) fn from_int(f: Fmt, x: i128) -> u128 {
    f.round_fixed(x, 0, Mode::Even)
}

/// `m` normalised to its top bit at bit 115, with its exponent.
fn widen(m: u128, e: i32) -> (u128, i32) {
    let s = m.leading_zeros() as i32 - 12;
    (m << s, e - s)
}

/// `m >> s`, its lowest bit set when a set bit is shifted out.
fn jam(m: u128, s: u32) -> u128 {
    if s >= 128 {
        u128::from(m != 0)
    } else {
        m >> s | u128::from(m & ((1 << s) - 1) != 0)
    }
}

pub(crate) fn add(f: Fmt, a: u128, b: u128) -> u128 {
    match (f.decode(a), f.decode(b)) {
        (Num::Nan, _) | (_, Num::Nan) => f.nan(),
        (Num::Inf(x), Num::Inf(y)) if x != y => f.nan(),
        (Num::Inf(x), _) | (_, Num::Inf(x)) => f.inf(x),
        (Num::Zero(x), Num::Zero(y)) => f.sign(x && y),
        (Num::Zero(_), _) => b,
        (_, Num::Zero(_)) => a,
        (
            Num::Fin {
                neg: na,
                m: ma,
                e: ea,
            },
            Num::Fin {
                neg: nb,
                m: mb,
                e: eb,
            },
        ) => {
            let ((ma, ea), (mb, eb)) = (widen(ma, ea), widen(mb, eb));
            let ((nx, mx, ex), (ny, my, ey)) = if ea >= eb {
                ((na, ma, ea), (nb, mb, eb))
            } else {
                ((nb, mb, eb), (na, ma, ea))
            };
            let my = jam(my, u32::try_from(ex - ey).expect("ordered"));
            if nx == ny {
                f.round(nx, mx + my, ex, Mode::Even)
            } else {
                match mx.cmp(&my) {
                    Ordering::Equal => f.sign(false),
                    Ordering::Greater => f.round(nx, mx - my, ex, Mode::Even),
                    Ordering::Less => f.round(ny, my - mx, ex, Mode::Even),
                }
            }
        }
    }
}

pub(crate) fn mul(f: Fmt, a: u128, b: u128) -> u128 {
    match (f.decode(a), f.decode(b)) {
        (Num::Nan, _)
        | (_, Num::Nan)
        | (Num::Inf(_), Num::Zero(_))
        | (Num::Zero(_), Num::Inf(_)) => f.nan(),
        (Num::Inf(x), Num::Inf(y)) => f.inf(x != y),
        (Num::Zero(x), Num::Zero(y)) => f.sign(x != y),
        (Num::Inf(x), Num::Fin { neg, .. }) | (Num::Fin { neg, .. }, Num::Inf(x)) => {
            f.inf(x != neg)
        }
        (Num::Zero(x), Num::Fin { neg, .. }) | (Num::Fin { neg, .. }, Num::Zero(x)) => {
            f.sign(x != neg)
        }
        (
            Num::Fin {
                neg: na,
                m: ma,
                e: ea,
            },
            Num::Fin {
                neg: nb,
                m: mb,
                e: eb,
            },
        ) => {
            // The 256-bit product from 64-bit limbs, jammed into 120 bits.
            let (ah, al) = (ma >> 64, ma & u128::from(u64::MAX));
            let (bh, bl) = (mb >> 64, mb & u128::from(u64::MAX));
            let (ll, lh, hl, hh) = (al * bl, al * bh, ah * bl, ah * bh);
            let mid = (ll >> 64) + (lh & u128::from(u64::MAX)) + (hl & u128::from(u64::MAX));
            let lo = (ll & u128::from(u64::MAX)) | (mid << 64);
            let hi = hh + (lh >> 64) + (hl >> 64) + (mid >> 64);
            let e = ea + eb;
            if hi == 0 {
                let s = 128u32
                    .saturating_sub(lo.leading_zeros())
                    .saturating_sub(120);
                f.round(na != nb, jam(lo, s), e + s as i32, Mode::Even)
            } else {
                let s = 256 - hi.leading_zeros() - 120;
                let m = hi << (128 - s) | lo >> s | u128::from(lo & ((1 << s) - 1) != 0);
                f.round(na != nb, m, e + s as i32, Mode::Even)
            }
        }
    }
}

pub(crate) fn div(f: Fmt, a: u128, b: u128) -> u128 {
    match (f.decode(a), f.decode(b)) {
        (Num::Nan, _)
        | (_, Num::Nan)
        | (Num::Inf(_), Num::Inf(_))
        | (Num::Zero(_), Num::Zero(_)) => f.nan(),
        (Num::Inf(x), Num::Fin { neg, .. } | Num::Zero(neg))
        | (Num::Fin { neg: x, .. }, Num::Zero(neg)) => f.inf(x != neg),
        (Num::Zero(x), Num::Fin { neg, .. } | Num::Inf(neg))
        | (Num::Fin { neg: x, .. }, Num::Inf(neg)) => f.sign(x != neg),
        (
            Num::Fin {
                neg: na,
                m: ma,
                e: ea,
            },
            Num::Fin {
                neg: nb,
                m: mb,
                e: eb,
            },
        ) => {
            // 120 quotient bits by long division, the remainder jammed in.
            let ((mut r, ea), (mb, eb)) = (widen(ma, ea), widen(mb, eb));
            let mut q = 0u128;
            for _ in 0..120 {
                q <<= 1;
                if r >= mb {
                    r -= mb;
                    q |= 1;
                }
                r <<= 1;
            }
            f.round(na != nb, q | u128::from(r != 0), ea - eb - 119, Mode::Even)
        }
    }
}

/// IEEE comparison; `None` when either is a NaN.
pub(crate) fn cmp(f: Fmt, a: u128, b: u128) -> Option<Ordering> {
    let key = |bits: u128| -> Option<i128> {
        match f.decode(bits) {
            Num::Nan => None,
            _ => {
                let mag = i128::try_from(bits & (f.sign(true) - 1)).expect("below the sign");
                Some(if bits & f.sign(true) != 0 { -mag } else { mag })
            }
        }
    };
    Some(key(a)?.cmp(&key(b)?))
}

pub(crate) fn neg(f: Fmt, a: u128) -> u128 {
    a ^ f.sign(true)
}
