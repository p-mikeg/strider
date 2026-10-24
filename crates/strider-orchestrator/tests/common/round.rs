//! Exact IEEE rounding by integer arithmetic, under every rounding mode: the
//! reference the conversion tests hold the lifted p-code to. A value is
//! `m * 2^e`; rounding divides `m` by a power of two and decides the
//! increment from the remainder.

/// A binary interchange format: exponent bits and stored fraction bits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Fmt {
    pub(crate) exp: u32,
    pub(crate) man: u32,
}

pub(crate) const HALF: Fmt = Fmt { exp: 5, man: 10 };
pub(crate) const BFLOAT: Fmt = Fmt { exp: 8, man: 7 };
pub(crate) const SINGLE: Fmt = Fmt { exp: 8, man: 23 };
pub(crate) const DOUBLE: Fmt = Fmt { exp: 11, man: 52 };

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Mode {
    Even,
    Up,
    Down,
    Zero,
    Odd,
}

/// The modes in the ARM and AArch64 RMode encoding: 0 nearest even, 1 toward
/// +inf, 2 toward -inf, 3 toward zero.
pub(crate) const ARM_MODES: [Mode; 4] = [Mode::Even, Mode::Up, Mode::Down, Mode::Zero];
/// MIPS FCSR.RM and PowerPC FPSCR.RN: 0 nearest even, 1 toward zero, 2 up,
/// 3 down.
pub(crate) const MIPS_MODES: [Mode; 4] = [Mode::Even, Mode::Zero, Mode::Up, Mode::Down];
/// x86 RC: 0 nearest even, 1 down, 2 up, 3 toward zero.
pub(crate) const X86_MODES: [Mode; 4] = [Mode::Even, Mode::Down, Mode::Up, Mode::Zero];

/// A decoded value; a finite nonzero one is `m * 2^e`.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Num {
    Nan,
    Inf(bool),
    Zero(bool),
    Fin { neg: bool, m: u128, e: i32 },
}

impl Fmt {
    pub(crate) fn bits(self) -> u32 {
        1 + self.exp + self.man
    }

    fn bias(self) -> i32 {
        (1 << (self.exp - 1)) - 1
    }

    fn sign(self, neg: bool) -> u128 {
        u128::from(neg) << (self.bits() - 1)
    }

    pub(crate) fn inf(self, neg: bool) -> u128 {
        self.sign(neg) | ((1 << self.exp) - 1) << self.man
    }

    fn max_finite(self, neg: bool) -> u128 {
        self.inf(neg) - 1
    }

    pub(crate) fn decode(self, bits: u128) -> Num {
        let neg = bits >> (self.bits() - 1) & 1 == 1;
        let ex = bits >> self.man & ((1 << self.exp) - 1);
        let fr = bits & ((1 << self.man) - 1);
        let emin = 1 - self.bias() - self.man as i32;
        match (ex, fr) {
            (x, 0) if x == (1 << self.exp) - 1 => Num::Inf(neg),
            (x, _) if x == (1 << self.exp) - 1 => Num::Nan,
            (0, 0) => Num::Zero(neg),
            (0, fr) => Num::Fin {
                neg,
                m: fr,
                e: emin,
            },
            (ex, fr) => Num::Fin {
                neg,
                m: fr | 1 << self.man,
                e: ex as i32 - 1 + emin,
            },
        }
    }

    pub(crate) fn is_nan(self, bits: u128) -> bool {
        matches!(self.decode(bits), Num::Nan)
    }

    /// `m * 2^e`, `m` nonzero, rounded to this format under `mode`.
    pub(crate) fn round(self, neg: bool, m: u128, e: i32, mode: Mode) -> u128 {
        assert!(m != 0);
        let p = self.man as i32 + 1;
        let lead = 127 - m.leading_zeros() as i32 + e;
        let mut q = lead.max(1 - self.bias()) - (p - 1);
        let sh = q - e;
        let (mut n, inexact, above_half, half) = if sh <= 0 {
            (m << -sh, false, false, false)
        } else if sh >= 128 {
            (0, true, false, false)
        } else {
            let rem = m & ((1 << sh) - 1);
            let h = 1u128 << (sh - 1);
            (m >> sh, rem != 0, rem > h, rem == h)
        };
        let up = match mode {
            Mode::Even => above_half || (half && n & 1 == 1),
            Mode::Up => inexact && !neg,
            Mode::Down => inexact && neg,
            Mode::Zero => false,
            Mode::Odd => inexact && n & 1 == 0,
        };
        n += u128::from(up);
        if n == 1 << p {
            n >>= 1;
            q += 1;
        }
        if q + p - 1 > self.bias() {
            let to_inf = match mode {
                Mode::Even => true,
                Mode::Up => !neg,
                Mode::Down => neg,
                Mode::Zero | Mode::Odd => false,
            };
            return if to_inf {
                self.inf(neg)
            } else {
                self.max_finite(neg)
            };
        }
        let field = if n >> (p - 1) == 0 {
            0
        } else {
            (q + p - 1 + self.bias()) as u128
        };
        self.sign(neg) | field << self.man | n & ((1 << self.man) - 1)
    }

    /// `bits` of format `src` converted to this format under `mode`; `None`
    /// for a NaN, whose encoding the caller checks itself.
    pub(crate) fn convert(self, src: Fmt, bits: u128, mode: Mode) -> Option<u128> {
        match src.decode(bits) {
            Num::Nan => None,
            Num::Inf(neg) => Some(self.inf(neg)),
            Num::Zero(neg) => Some(self.sign(neg)),
            Num::Fin { neg, m, e } => Some(self.round(neg, m, e, mode)),
        }
    }

    /// The integer `x / 2^fbits` in this format under `mode`; +0 for 0.
    pub(crate) fn round_fixed(self, x: i128, fbits: i32, mode: Mode) -> u128 {
        if x == 0 {
            0
        } else {
            self.round(x < 0, x.unsigned_abs(), -fbits, mode)
        }
    }

    /// The bit patterns next above and below `bits` in magnitude order.
    pub(crate) fn neighbours(self, bits: u128) -> [u128; 2] {
        let sign = self.sign(true);
        let mag = bits & !sign;
        let s = bits & sign;
        let up = s | (mag + 1).min(self.inf(false));
        let down = if mag == 0 {
            sign ^ s | 1
        } else {
            s | (mag - 1)
        };
        [up, down]
    }
}

/// Bit patterns of `src` around `dst`'s rounding boundaries: `dst`'s own
/// values, the midpoints between neighbours, the `src` neighbours of both,
/// values past `dst`'s range and below its smallest denormal, signed zeros,
/// infinities and NaNs.
pub(crate) fn narrowing_inputs(src: Fmt, dst: Fmt) -> Vec<u128> {
    let mut targets = vec![
        0,
        1,
        2,
        3,
        (1 << dst.man) - 1,
        1 << dst.man,
        (1 << dst.man) + 1,
        dst.max_finite(false),
        dst.max_finite(false) - 1,
    ];
    let bias = dst.bias() as u128;
    for ex in [bias, bias + 1, bias - 1, bias + 3, 2, (1 << dst.exp) - 3] {
        for fr in [
            0,
            1,
            2,
            3,
            (1 << dst.man) - 1,
            1 << (dst.man - 1),
            0x55 & ((1 << dst.man) - 1),
        ] {
            targets.push(ex << dst.man | fr);
        }
    }
    let mut out = Vec::new();
    for t in targets {
        for neg in [false, true] {
            let t = t | dst.sign(neg);
            let Num::Fin { neg, m, e } = dst.decode(t) else {
                continue;
            };
            // t, and the midpoint to the next value away from zero
            for (mm, ee) in [(m, e), (2 * m + 1, e - 1)] {
                let b = src.round(neg, mm, ee, Mode::Even);
                out.push(b);
                out.extend(src.neighbours(b));
            }
        }
    }
    // past the range: the overflow threshold, twice the largest value
    if let Num::Fin { m, e, .. } = dst.decode(dst.max_finite(false)) {
        for neg in [false, true] {
            for (mm, ee) in [(2 * m + 1, e - 1), (m, e + 1)] {
                let b = src.round(neg, mm, ee, Mode::Even);
                out.push(b);
                out.extend(src.neighbours(b));
            }
        }
    }
    // far below the smallest denormal
    for b in [1, 0x10, 1 << (src.man - 1)] {
        out.extend([b, b | src.sign(true)]);
    }
    let nan = src.inf(false);
    out.extend([
        0,
        src.sign(true),
        src.inf(false),
        src.inf(true),
        nan | 1 << (src.man - 1),
        nan | 1 << (src.man - 1) | 0x123,
        nan | 1,
        src.sign(true) | nan | 1 << (src.man - 1),
    ]);
    out.sort_unstable();
    out.dedup();
    out
}

/// `bits`-wide integers around `dst`'s precision and range: powers of two
/// and multiples of three times them, plus or minus a half ulp and one, and
/// the extremes. Signed ones in both signs.
pub(crate) fn integer_inputs(bits: u32, signed: bool, dst: Fmt) -> Vec<i128> {
    let (lo, hi): (i128, i128) = if signed {
        (-(1 << (bits - 1)), (1 << (bits - 1)) - 1)
    } else {
        (0, (1 << bits) - 1)
    };
    let p = dst.man as i32 + 1;
    let mut out = vec![0, 1, 2, 3, lo, hi, lo + 1, hi - 1];
    for s in (p - 1).max(1)..=bits as i32 {
        let base: i128 = 1 << s;
        let half: i128 = 1 << (s - p).max(0);
        for k in [1, 3] {
            for d in [
                -half - 1,
                -half,
                -half + 1,
                -1,
                0,
                1,
                half - 1,
                half,
                half + 1,
            ] {
                out.push(k * base + d);
            }
        }
    }
    if let Num::Fin { m, e, .. } = dst.decode(dst.max_finite(false))
        && e >= 0
        && (e as u32) < bits
    {
        let top = (m as i128) << e;
        let ulp: i128 = 1 << e;
        out.extend([
            top,
            top + ulp / 2 - 1,
            top + ulp / 2,
            top + ulp / 2 + 1,
            top + ulp,
        ]);
    }
    if signed {
        let neg: Vec<i128> = out.iter().map(|v| -v).collect();
        out.extend(neg);
    }
    out.retain(|v| (lo..=hi).contains(v));
    out.sort_unstable();
    out.dedup();
    out
}
