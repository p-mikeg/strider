//! The interval lattice [`RangeMap`](super::RangeMap) carries, and the
//! constant scalings it maps through.

use strider_ir::IRViewer;
use strider_ir::eval::bit;
use strider_ir::node::{ValueId, ValueType};

/// `gcd(0, x) == x`: a side with no spacing takes the other's.
fn gcd(mut a: u128, mut b: u128) -> u128 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

/// The empty set (`hi < lo`).
pub(super) const EMPTY: Interval = Interval::new(1, 0, 1);

/// The inclusive unsigned value set `{ lo, lo+stride, ... hi }`. `stride`
/// always divides the real spacing.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) struct Interval {
    pub(super) lo: u128,
    pub(super) hi: u128,
    /// At least 1.
    pub(super) stride: u128,
}

impl Interval {
    /// `{ lo, lo+stride, ... hi }`; a `stride` of 0 is read as 1.
    pub(crate) const fn new(lo: u128, hi: u128, stride: u128) -> Self {
        Self {
            lo,
            hi,
            stride: if stride == 0 { 1 } else { stride },
        }
    }

    pub(crate) fn lo(&self) -> u128 {
        self.lo
    }

    pub(crate) fn hi(&self) -> u128 {
        self.hi
    }

    pub(crate) fn stride(&self) -> u128 {
        self.stride
    }

    /// `hi < lo`.
    pub(crate) fn is_empty(&self) -> bool {
        self.hi < self.lo
    }

    pub(crate) fn top(width_mask: u128) -> Self {
        Self::dense(0, width_mask)
    }

    pub(crate) fn dense(lo: u128, hi: u128) -> Self {
        Self::new(lo, hi, 1)
    }

    /// A stride is a fact beyond the span, so a strided full-span interval is
    /// not top.
    pub(crate) fn is_top(&self, width_mask: u128) -> bool {
        self.lo == 0 && self.hi >= width_mask && self.stride == 1
    }

    /// Whether `x` is in the set.
    pub(crate) fn contains(&self, x: u128) -> bool {
        self.lo <= x && x <= self.hi && (x - self.lo).is_multiple_of(self.stride)
    }

    /// Whether every value of `other` is in the set.
    pub(super) fn covers(&self, other: Self) -> bool {
        other.is_empty()
            || (self.contains(other.lo)
                && other.hi <= self.hi
                && (other.count() == 1 || other.stride.is_multiple_of(self.stride)))
    }

    /// Number of values in the set.  `0` if `hi < lo`.  Saturates: a full-width
    /// top spans `u128::MAX + 1` values.
    pub(crate) fn count(&self) -> u128 {
        if self.is_empty() {
            0
        } else {
            ((self.hi - self.lo) / self.stride).saturating_add(1)
        }
    }

    /// Meet of two arithmetic progressions, stepping by `lcm(s1, s2)`; empty
    /// for incompatible residues. Where `lcm` overflows or the residue search
    /// is capped, the wider stride's progression alone, a superset.
    pub(super) fn intersect(self, other: Self) -> Self {
        // `a` takes the wider stride, so the answer is order-independent.
        let (a, b) = if self.stride >= other.stride {
            (self, other)
        } else {
            (other, self)
        };
        let lo_bound = a.lo.max(b.lo);
        let hi = a.hi.min(b.hi);
        // The first `x >= lo_bound` with `x = a.lo (mod a.stride)`.
        let off = (lo_bound - a.lo) % a.stride;
        let Some(x0) = lo_bound.checked_add((a.stride - off) % a.stride) else {
            return EMPTY;
        };
        if x0 > hi {
            return EMPTY;
        }
        const MAX_CRT_STEPS: u128 = 64;
        let steps = (a.stride / gcd(a.stride, b.stride))
            .checked_mul(b.stride)
            .map(|lcm| lcm / a.stride);
        let mut x = x0;
        for _ in 0..steps.unwrap_or(MAX_CRT_STEPS).min(MAX_CRT_STEPS) {
            if (x - b.lo).is_multiple_of(b.stride) {
                return Self::new(x, hi, steps.map_or(a.stride, |n| n * a.stride));
            }
            match x.checked_add(a.stride) {
                Some(next) if next <= hi => x = next,
                _ => return EMPTY,
            }
        }
        if steps.is_some_and(|n| n <= MAX_CRT_STEPS) {
            return EMPTY;
        }
        Self::new(x0, hi, a.stride)
    }

    /// Join of two arithmetic progressions, its stride the gcd of both strides
    /// and the gap between their starts. A side of at most one element
    /// contributes no stride of its own.
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

    /// `{ x * c : x in self }`; top on overflow or `c == 0`.
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

    /// `{ x / c : x in self }` (`c == 0` read as 1); the stride is
    /// `stride / c` when `c` divides it, else 1.
    pub(super) fn udiv(self, c: u128) -> Self {
        // Mapped endpoint-wise, an empty interval could come back `{0, 0}`.
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

/// A monotone constant scaling of one variable operand.
#[derive(Clone, Copy)]
pub(crate) enum ScaleOp {
    Mul(u128),
    Udiv(u128),
}

impl ScaleOp {
    /// `x << k`, `None` past the `u128` carrier.
    pub(super) fn shl(k: u32) -> Option<Self> {
        (k < 128).then(|| ScaleOp::Mul(bit(k)))
    }

    /// `x >> k`, `None` past the `u128` carrier.
    pub(super) fn shr(k: u32) -> Option<Self> {
        (k < 128).then(|| ScaleOp::Udiv(bit(k)))
    }

    pub(super) fn apply(self, iv: Interval, width_mask: u128) -> Interval {
        match self {
            ScaleOp::Mul(c) => iv.mul(c, width_mask),
            ScaleOp::Udiv(c) => iv.udiv(c),
        }
    }
}

/// The mask of `v`'s width; see [`type_mask_or_top`].
pub(crate) fn width_mask(f: &strider_ir::Function, v: ValueId) -> u128 {
    type_mask_or_top(f.value_type_opt(v))
}

/// `ty`'s integer mask, or `u128::MAX` (top, no bound) for no type, a
/// non-integer or a width past `u128`. A consumer needing a real bound gates
/// on [`ValueType::int_mask_u128`].
pub(super) fn type_mask_or_top(ty: Option<ValueType>) -> u128 {
    ty.and_then(ValueType::int_mask_u128).unwrap_or(u128::MAX)
}
