//! IEEE binary floats as bit patterns, rounded to nearest even: the cone's
//! 128-bit floats, which no host type holds. NaN payloads are unspecified.

use std::cmp::Ordering;

/// A binary interchange format: stored fraction bits and exponent bits.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Format {
    man: u32,
    exp: u32,
}

pub(crate) const HALF: Format = Format { man: 10, exp: 5 };
pub(crate) const SINGLE: Format = Format { man: 23, exp: 8 };
pub(crate) const DOUBLE: Format = Format { man: 52, exp: 11 };
pub(crate) const QUAD: Format = Format { man: 112, exp: 15 };

/// A decoded value; a finite nonzero one is `m * 2^e`.
#[derive(Clone, Copy, Debug)]
enum Num {
    Nan,
    Inf(bool),
    Zero(bool),
    Fin { neg: bool, m: u128, e: i32 },
}

impl Format {
    fn bits(self) -> u32 {
        1 + self.exp + self.man
    }

    fn bias(self) -> i32 {
        (1 << (self.exp - 1)) - 1
    }

    fn sign(self, neg: bool) -> u128 {
        u128::from(neg) << (self.bits() - 1)
    }

    fn max_exp(self) -> u128 {
        (1 << self.exp) - 1
    }

    pub(crate) fn nan(self) -> u128 {
        self.max_exp() << self.man | 1 << (self.man - 1)
    }

    fn inf(self, neg: bool) -> u128 {
        self.sign(neg) | self.max_exp() << self.man
    }

    fn decode(self, bits: u128) -> Num {
        let neg = bits >> (self.bits() - 1) & 1 == 1;
        let ex = bits >> self.man & self.max_exp();
        let fr = bits & ((1 << self.man) - 1);
        let emin = 1 - self.bias() - self.man as i32;
        if ex == self.max_exp() {
            if fr == 0 { Num::Inf(neg) } else { Num::Nan }
        } else if ex == 0 {
            if fr == 0 {
                Num::Zero(neg)
            } else {
                Num::Fin {
                    neg,
                    m: fr,
                    e: emin,
                }
            }
        } else {
            Num::Fin {
                neg,
                m: fr | 1 << self.man,
                e: ex as i32 - 1 + emin,
            }
        }
    }

    /// `m * 2^e` rounded to this format. Below two bits past the precision,
    /// `m`'s lowest bit may stand for any nonzero remainder.
    fn round(self, neg: bool, m: u128, e: i32) -> u128 {
        if m == 0 {
            return self.sign(neg);
        }
        let p = self.man as i32 + 1;
        let top = e + 127 - m.leading_zeros() as i32;
        let lsb = (top - (p - 1)).max(1 - self.bias() - (p - 1));
        let mut r = if lsb <= e {
            m << (e - lsb)
        } else {
            let s = u32::try_from(lsb - e).expect("a positive shift");
            if s > 128 {
                0
            } else {
                let half = 1u128 << (s - 1);
                let rem = if s == 128 { m } else { m & ((1 << s) - 1) };
                let r = if s == 128 { 0 } else { m >> s };
                r + u128::from(rem > half || (rem == half && r & 1 == 1))
            }
        };
        let mut q = lsb;
        if r == 1 << p {
            r >>= 1;
            q += 1;
        }
        if r == 0 {
            return self.sign(neg);
        }
        let lead = q + 127 - r.leading_zeros() as i32;
        if lead > self.bias() {
            return self.inf(neg);
        }
        let body = if r >> (p - 1) == 0 {
            r
        } else {
            u128::try_from(lead + self.bias()).expect("a normal exponent") << self.man
                | r & ((1 << self.man) - 1)
        };
        self.sign(neg) | body
    }
}

/// `bits` of `from` converted to `to`.
pub(crate) fn convert(from: Format, to: Format, bits: u128) -> u128 {
    match from.decode(bits) {
        Num::Nan => to.nan(),
        Num::Inf(neg) => to.inf(neg),
        Num::Zero(neg) => to.sign(neg),
        Num::Fin { neg, m, e } => to.round(neg, m, e),
    }
}

/// The signed integer `x` in `f`.
pub(crate) fn from_int(f: Format, x: i128) -> u128 {
    f.round(x < 0, x.unsigned_abs(), 0)
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

pub(crate) fn add(f: Format, a: u128, b: u128) -> u128 {
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
                f.round(nx, mx + my, ex)
            } else {
                match mx.cmp(&my) {
                    Ordering::Equal => f.sign(false),
                    Ordering::Greater => f.round(nx, mx - my, ex),
                    Ordering::Less => f.round(ny, my - mx, ex),
                }
            }
        }
    }
}

pub(crate) fn mul(f: Format, a: u128, b: u128) -> u128 {
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
                f.round(na != nb, jam(lo, s), e + s as i32)
            } else {
                let s = 256 - hi.leading_zeros() - 120;
                let m = hi << (128 - s) | lo >> s | u128::from(lo & ((1 << s) - 1) != 0);
                f.round(na != nb, m, e + s as i32)
            }
        }
    }
}

pub(crate) fn div(f: Format, a: u128, b: u128) -> u128 {
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
            f.round(na != nb, q | u128::from(r != 0), ea - eb - 119)
        }
    }
}

/// IEEE comparison; `None` when either is a NaN.
pub(crate) fn cmp(f: Format, a: u128, b: u128) -> Option<Ordering> {
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

pub(crate) fn neg(f: Format, a: u128) -> u128 {
    a ^ f.sign(true)
}

pub(crate) fn is_nan(f: Format, a: u128) -> bool {
    matches!(f.decode(a), Num::Nan)
}
