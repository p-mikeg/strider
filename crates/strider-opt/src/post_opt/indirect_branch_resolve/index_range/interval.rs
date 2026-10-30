//! The interval lattice: the value set [`RangeMap`](super::RangeMap) carries,
//! the maps one hop up a value chain puts on it, and the width masks they are
//! read against.

use strider_ir::IRViewer;
use strider_ir::node::{ValueId, ValueType};

/// How much of an operand's bound survives one hop up to the value built from
/// it.  [`bound_carrying_operand`](super::RangeMap::bound_carrying_operand) yields every variant but
/// `Scaled`, which only [`arm_carrying_operand`](super::RangeMap::arm_carrying_operand) adds.
#[derive(Clone, Copy)]
pub(super) enum Hop {
    /// Identical unsigned value: the whole interval carries.
    SameValue,
    /// Identical below this mask, which the operand's `hi` must fit.
    SameValueBelow(u128),
    /// Only `hi` carries; the low end and the stride do not.
    UpperBound,
    /// A monotone constant scaling, at the result's width mask.
    Scaled(ScaleOp, u128),
}

impl Hop {
    /// The map this hop puts on the operand's interval.
    pub(super) fn lift(self, iv: Interval) -> Option<Interval> {
        match self {
            Hop::SameValue => Some(iv),
            Hop::SameValueBelow(mask) => (iv.hi <= mask).then_some(iv),
            Hop::UpperBound => Some(Interval::dense(0, iv.hi)),
            Hop::Scaled(scale, mask) => Some(scale.apply(iv, mask)),
        }
    }
}

/// `gcd(0, x) == x` and `gcd(0, 0) == 0`, the identity: a side contributing no
/// spacing must take the other's, not flatten it.  Every caller clamps the
/// result where a divisor of zero would be meaningless.
fn gcd(mut a: u128, mut b: u128) -> u128 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

/// The empty set (`hi < lo`).
pub(super) const EMPTY: Interval = Interval::new(1, 0, 1);

/// The inclusive value set `{ lo, lo+stride, lo+2*stride, ... hi }`.
///
/// `lo`/`hi` are unsigned regardless of signedness: guard extraction gates
/// `Sless` on KnownBits sign-bit = 0, so the interval is non-negative.
///
/// `stride` is a must-congruence, from KnownBits trailing zeros or carried
/// through a union, a meet or a constant `Mul` / `ShiftLeft`, and always a
/// sound divisor of the real spacing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Interval {
    pub(super) lo: u128,
    pub(super) hi: u128,
    /// At least 1.
    pub(super) stride: u128,
}

impl Interval {
    /// `{ lo, lo+stride, ... hi }`; a `stride` of 0 is read as 1.
    pub const fn new(lo: u128, hi: u128, stride: u128) -> Self {
        Self {
            lo,
            hi,
            stride: if stride == 0 { 1 } else { stride },
        }
    }

    pub fn lo(&self) -> u128 {
        self.lo
    }

    pub fn hi(&self) -> u128 {
        self.hi
    }

    pub fn stride(&self) -> u128 {
        self.stride
    }

    /// `hi < lo`.
    pub fn is_empty(&self) -> bool {
        self.hi < self.lo
    }

    pub fn top(width_mask: u128) -> Self {
        Self::dense(0, width_mask)
    }

    pub fn dense(lo: u128, hi: u128) -> Self {
        Self::new(lo, hi, 1)
    }

    /// A stride is a fact beyond the span, so a strided full-span interval is
    /// not top.
    pub fn is_top(&self, width_mask: u128) -> bool {
        self.lo == 0 && self.hi >= width_mask && self.stride == 1
    }

    /// Whether `x` is in the set.
    pub fn contains(&self, x: u128) -> bool {
        self.lo <= x && x <= self.hi && (x - self.lo).is_multiple_of(self.stride)
    }

    /// Number of values in the set.  `0` if `hi < lo`.  Saturates: a full-width
    /// top spans `u128::MAX + 1` values.
    pub fn count(&self) -> u128 {
        if self.is_empty() {
            0
        } else {
            ((self.hi - self.lo) / self.stride).saturating_add(1)
        }
    }

    /// Meet of two arithmetic progressions. The result stride is `lcm(s1, s2)`
    /// (1 where that overflows or the CRT walk is capped) and `lo` is the first
    /// element `>= max(lo1, lo2)` congruent to both
    /// sources; incompatible residues (or an empty range) yield an empty
    /// interval (`count() == 0`). Sound: the result always contains the real
    /// value set, which is a subset of both operands.
    pub(super) fn intersect(self, other: Self) -> Self {
        let lo_bound = self.lo.max(other.lo);
        let hi = self.hi.min(other.hi);
        if lo_bound > hi {
            return EMPTY;
        }
        let s1 = self.stride;
        let s2 = other.stride;
        if s1 == 1 && s2 == 1 {
            return Self::dense(lo_bound, hi);
        }
        // stride = lcm(s1, s2); on overflow fall back to the sound dense meet.
        let Some(lcm) = (s1 / gcd(s1, s2)).checked_mul(s2) else {
            return Self::dense(lo_bound, hi);
        };
        // With `s1 == 1` every integer satisfies the first congruence, so the
        // answer is the first element at or above `lo_bound` satisfying the
        // second.  Closed form, where the walk below would be `s2/gcd(s1,s2)`
        // steps: billions against a KnownBits stride of `2^32`.
        if s1 == 1 {
            // As a difference of residues, never as `r + s2 - r2`: `s2` may be
            // past half the `u128` carrier.
            let (r, r_bound) = (other.lo % s2, lo_bound % s2);
            let delta = if r >= r_bound {
                r - r_bound
            } else {
                s2 - (r_bound - r)
            };
            return match lo_bound.checked_add(delta) {
                Some(x) if x <= hi => Self::new(x, hi, s2),
                _ => EMPTY,
            };
        }
        // Otherwise cap the walk and fall back to the dense meet: surplus values
        // become dead CFG edges, never a dropped real one.
        const MAX_CRT_STEPS: u128 = 64;
        if lcm / s1 > MAX_CRT_STEPS {
            return Self::dense(lo_bound, hi);
        }
        // First x >= lo_bound with x = self.lo (mod s1); then step by s1 up to
        // lcm/s1 times to hit the second congruence (exactly one match per lcm
        // span, or none when the residues are incompatible).
        let off = (lo_bound - self.lo) % s1;
        let Some(mut x) = (if off == 0 {
            Some(lo_bound)
        } else {
            lo_bound.checked_add(s1 - off)
        }) else {
            return EMPTY;
        };
        for _ in 0..(lcm / s1) {
            if (x - other.lo).is_multiple_of(s2) {
                return if x <= hi {
                    Self::new(x, hi, lcm)
                } else {
                    EMPTY
                };
            }
            let Some(next) = x.checked_add(s1) else {
                return EMPTY;
            };
            x = next;
        }
        EMPTY
    }

    /// Join of two arithmetic progressions.  The stride survives as the gcd of
    /// both strides and the gap between their starts, which is what every
    /// element of either side is congruent to.  A side holding at most one
    /// element constrains no spacing and takes the other's: a phi arm pinned to
    /// a constant must not flatten the strided arm it joins, and two of them
    /// keep the gap between their starts.  Zero survives only where the join is
    /// one point, where 1 is as tight.  An empty side adds nothing.
    pub(super) fn union(self, other: Self) -> Self {
        if self.is_empty() {
            return other;
        }
        if other.is_empty() {
            return self;
        }
        let step = |iv: Self| if iv.count() <= 1 { 0 } else { iv.stride };
        Self::new(
            self.lo.min(other.lo),
            self.hi.max(other.hi),
            gcd(gcd(step(self), step(other)), self.lo.abs_diff(other.lo)),
        )
    }

    /// `{ x << k : x in self }` = `{ x * 2^k }`.  Routed through `mul` so the
    /// overflow guard is `checked_mul` on the value; `checked_shl` only rejects
    /// `k >= 128` and otherwise wraps bits out of the u128 carrier.
    pub(super) fn shl(self, k: u32, width_mask: u128) -> Self {
        match 1u128.checked_shl(k) {
            Some(factor) => self.mul(factor, width_mask),
            None => Self::top(width_mask),
        }
    }

    /// `{ x * c : x in self }` for `c > 0`.  Exact, or top on overflow / `c == 0`.
    pub(super) fn mul(self, c: u128, width_mask: u128) -> Self {
        match (
            self.lo.checked_mul(c),
            self.hi.checked_mul(c),
            self.stride.checked_mul(c),
        ) {
            (Some(lo), Some(hi), Some(stride)) if c != 0 && hi <= width_mask => {
                Self::new(lo, hi, stride)
            }
            _ => Self::top(width_mask),
        }
    }

    /// `{ x >> k : x in self }`.  Floor maps the endpoints directly.  The stride
    /// survives as `stride >> k` when `2^k` divides it (low `k` bits zero), else
    /// floor breaks the progression and it widens to 1, never a stride tighter
    /// than the real spacing.
    pub(super) fn shr(self, k: u32) -> Self {
        // Both endpoints map independently, so an empty interval would come
        // back as `{0, 0}`, "exactly zero", and pass `bounded_index`'s gate.
        if self.is_empty() {
            return self;
        }
        let stride = if self.stride.trailing_zeros() >= k {
            self.stride >> k
        } else {
            1
        };
        Self::new(
            self.lo.checked_shr(k).unwrap_or(0),
            self.hi.checked_shr(k).unwrap_or(0),
            stride,
        )
    }

    /// `{ x / c : x in self }` (`c == 0` treated as 1).  Floor maps the endpoints
    /// directly.  The stride survives as `stride / c` when `c` divides it, else
    /// floor breaks the progression and it widens to 1, never a stride tighter
    /// than the real spacing.
    pub(super) fn udiv(self, c: u128) -> Self {
        // As in `shr`: floor-mapping both endpoints would resurrect EMPTY.
        if self.is_empty() {
            return self;
        }
        let d = c.max(1);
        let stride = if self.stride.is_multiple_of(d) {
            self.stride / d
        } else {
            1
        };
        Self::new(self.lo / d, self.hi / d, stride)
    }
}

/// A monotone constant scaling of one variable operand: forward-propagating a
/// bound on the operand through it yields a bound on the result.
#[derive(Clone, Copy)]
pub(super) enum ScaleOp {
    Shl(u32),
    Shr(u32),
    Mul(u128),
    Udiv(u128),
}

impl ScaleOp {
    pub(super) fn apply(self, iv: Interval, width_mask: u128) -> Interval {
        match self {
            ScaleOp::Shl(k) => iv.shl(k, width_mask),
            ScaleOp::Shr(k) => iv.shr(k),
            ScaleOp::Mul(c) => iv.mul(c, width_mask),
            ScaleOp::Udiv(c) => iv.udiv(c),
        }
    }
}

/// The mask of `v`'s width; see [`type_mask_or_top`].
pub(super) fn width_mask(f: &strider_ir::Function, v: ValueId) -> u128 {
    type_mask_or_top(f.value_type_opt(v))
}

/// `u128::MAX` (unconstrained top) when the value carries no typed edge, for a
/// non-integer type, and for a width past the `u128` carrier, where no `u128`
/// is a bound the value is inside. Every consumer that needs a real bound gates
/// on [`ValueType::int_mask_u128`], which answers `None` in all three cases.
pub(super) fn type_mask_or_top(ty: Option<ValueType>) -> u128 {
    ty.and_then(ValueType::int_mask_u128).unwrap_or(u128::MAX)
}
