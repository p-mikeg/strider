//! The interval lattice: the value set [`RangeMap`](super::RangeMap) carries,
//! the maps one hop up a value chain puts on it, and the types they are read
//! against.

use strider_ir::eval;
use strider_ir::node::{ValueId, ValueType};
use strider_ir::{IRViewer, UBig};

/// The widest supported integer type.
pub(crate) fn widest_int() -> ValueType {
    ValueType::SUPPORTED
        .into_iter()
        .filter(|t| t.is_integer())
        .max_by_key(|t| t.bit_width())
        .expect("an integer width is supported")
}

/// Every value any supported integer type holds: the bound on a value whose
/// own width is unknown.
pub(super) fn unbounded() -> UBig {
    eval::all_ones(widest_int())
}

/// The greatest value `v` holds; see [`type_bound`].
pub(super) fn value_bound(f: &strider_ir::Function, v: ValueId) -> UBig {
    type_bound(f.value_type_opt(v))
}

/// `ty`'s all-ones, and [`unbounded`] where the value carries no typed edge or
/// a float type, for neither of which is a narrower value a bound it is
/// inside. A float's own all-ones is zero, which would read as "exactly 0".
pub(super) fn type_bound(ty: Option<ValueType>) -> UBig {
    match ty {
        Some(t) if t.is_integer() => eval::all_ones(t),
        _ => unbounded(),
    }
}

/// How much of an operand's bound survives one hop up to the value built from
/// it.  [`bound_carrying_operand`](super::RangeMap::bound_carrying_operand) yields every variant but
/// `Scaled`, which only [`arm_carrying_operand`](super::RangeMap::arm_carrying_operand) adds.
#[derive(Clone)]
pub(super) enum Hop {
    /// Identical unsigned value: the whole interval carries.
    SameValue,
    /// Identical below this integer type's all-ones, which the operand's `hi`
    /// must fit.
    SameValueBelow(ValueType),
    /// Only `hi` carries; the low end and the stride do not.
    UpperBound,
    /// A monotone constant scaling, at the result's bound.
    Scaled(ScaleOp, UBig),
}

impl Hop {
    /// The map this hop puts on the operand's interval.
    pub(super) fn lift(&self, iv: Interval) -> Option<Interval> {
        match self {
            Hop::SameValue => Some(iv),
            Hop::SameValueBelow(ty) => (*iv.hi() <= eval::all_ones(*ty)).then_some(iv),
            Hop::UpperBound => Some(Interval::dense(UBig::ZERO, iv.hi().clone())),
            Hop::Scaled(scale, bound) => Some(scale.apply(iv, bound)),
        }
    }
}

/// `gcd(0, x) == x` and `gcd(0, 0) == 0`, the identity: a side contributing no
/// spacing must take the other's, not flatten it.  Every caller clamps the
/// result where a divisor of zero would be meaningless.
fn gcd(mut a: UBig, mut b: UBig) -> UBig {
    while b != UBig::ZERO {
        let r = a % &b;
        a = b;
        b = r;
    }
    a
}

/// `v`'s trailing one bits.  Not `UBig::trailing_ones`, which indexes past its
/// own word buffer on an all-ones value of 192 bits or more (dashu-int 0.4.3).
pub(super) fn trailing_ones(v: &UBig) -> usize {
    (v + UBig::ONE)
        .trailing_zeros()
        .expect("v + 1 is never zero")
}

fn abs_diff(a: &UBig, b: &UBig) -> UBig {
    if a > b { a - b } else { b - a }
}

/// `a + b` in `ty`'s two's-complement ring.
pub(crate) fn add_at(a: &UBig, b: &UBig, ty: ValueType) -> UBig {
    eval::mask(&(a + b), ty)
}

/// `a - b` in `ty`'s two's-complement ring.
pub(crate) fn sub_at(a: &UBig, b: &UBig, ty: ValueType) -> UBig {
    eval::mask(&(a + eval::complement(b, ty) + UBig::ONE), ty)
}

/// The inclusive value set `{ lo, lo+stride, lo+2*stride, ... hi }`.
///
/// `lo`/`hi` are unsigned regardless of signedness: guard extraction gates
/// `Sless` on KnownBits sign-bit = 0, so the interval is non-negative.
///
/// `stride` is a must-congruence, from KnownBits trailing zeros or carried
/// through a union, a meet or a constant `Mul` / `ShiftLeft`, and always a
/// sound divisor of the real spacing.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Interval {
    pub(super) lo: UBig,
    pub(super) hi: UBig,
    /// At least 1.
    pub(super) stride: UBig,
}

impl Interval {
    /// `{ lo, lo+stride, ... hi }`; a `stride` of 0 is read as 1.
    pub fn new(lo: UBig, hi: UBig, stride: UBig) -> Self {
        Self {
            lo,
            hi,
            stride: if stride == UBig::ZERO {
                UBig::ONE
            } else {
                stride
            },
        }
    }

    /// The empty set (`hi < lo`).
    pub(super) fn empty() -> Self {
        Self::new(UBig::ONE, UBig::ZERO, UBig::ONE)
    }

    pub fn lo(&self) -> &UBig {
        &self.lo
    }

    pub fn hi(&self) -> &UBig {
        &self.hi
    }

    pub fn stride(&self) -> &UBig {
        &self.stride
    }

    /// `hi < lo`.
    pub fn is_empty(&self) -> bool {
        self.hi < self.lo
    }

    /// Every value at or below `bound`.
    pub fn top(bound: &UBig) -> Self {
        Self::dense(UBig::ZERO, bound.clone())
    }

    pub fn dense(lo: UBig, hi: UBig) -> Self {
        Self::new(lo, hi, UBig::ONE)
    }

    /// A stride is a fact beyond the span, so a strided full-span interval is
    /// not top.
    pub fn is_top(&self, bound: &UBig) -> bool {
        self.lo == UBig::ZERO && self.hi >= *bound && self.stride == UBig::ONE
    }

    /// Whether `x` is in the set.
    pub fn contains(&self, x: &UBig) -> bool {
        self.lo <= *x && *x <= self.hi && (x - &self.lo) % &self.stride == UBig::ZERO
    }

    /// Number of values in the set.  `0` if `hi < lo`.
    pub fn count(&self) -> UBig {
        if self.is_empty() {
            UBig::ZERO
        } else {
            (&self.hi - &self.lo) / &self.stride + UBig::ONE
        }
    }

    /// Meet of two arithmetic progressions. The result stride is `lcm(s1, s2)`
    /// (1 where the CRT walk is capped) and `lo` is the first element
    /// `>= max(lo1, lo2)` congruent to both
    /// sources; incompatible residues (or an empty range) yield an empty
    /// interval (`count() == 0`). Sound: the result always contains the real
    /// value set, which is a subset of both operands.
    pub(super) fn intersect(self, other: Self) -> Self {
        let lo_bound = self.lo.clone().max(other.lo.clone());
        let hi = self.hi.clone().min(other.hi.clone());
        if lo_bound > hi {
            return Self::empty();
        }
        let s1 = self.stride;
        let s2 = other.stride;
        if s1 == UBig::ONE && s2 == UBig::ONE {
            return Self::dense(lo_bound, hi);
        }
        let lcm = &s1 / gcd(s1.clone(), s2.clone()) * &s2;
        // With `s1 == 1` every integer satisfies the first congruence, so the
        // answer is the first element at or above `lo_bound` satisfying the
        // second.  Closed form, where the walk below would be `s2/gcd(s1,s2)`
        // steps: billions against a KnownBits stride of `2^32`.
        if s1 == UBig::ONE {
            let (r, r_bound) = (&other.lo % &s2, &lo_bound % &s2);
            let delta = if r >= r_bound {
                r - r_bound
            } else {
                &s2 - (r_bound - r)
            };
            let x = lo_bound + delta;
            return if x <= hi {
                Self::new(x, hi, s2)
            } else {
                Self::empty()
            };
        }
        // Otherwise cap the walk and fall back to the dense meet: surplus values
        // become dead CFG edges, never a dropped real one.
        const MAX_CRT_STEPS: u32 = 64;
        let steps = &lcm / &s1;
        if steps > UBig::from(MAX_CRT_STEPS) {
            return Self::dense(lo_bound, hi);
        }
        // First x >= lo_bound with x = self.lo (mod s1); then step by s1 up to
        // lcm/s1 times to hit the second congruence (exactly one match per lcm
        // span, or none when the residues are incompatible).
        let off = (&lo_bound - &self.lo) % &s1;
        let mut x = if off == UBig::ZERO {
            lo_bound
        } else {
            lo_bound + (&s1 - off)
        };
        let steps = u32::try_from(&steps).expect("capped at MAX_CRT_STEPS");
        for _ in 0..steps {
            if (&x - &other.lo) % &s2 == UBig::ZERO {
                return if x <= hi {
                    Self::new(x, hi, lcm)
                } else {
                    Self::empty()
                };
            }
            x += &s1;
        }
        Self::empty()
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
        let step = |iv: &Self| {
            if iv.count() <= UBig::ONE {
                UBig::ZERO
            } else {
                iv.stride.clone()
            }
        };
        let spacing = gcd(
            gcd(step(&self), step(&other)),
            abs_diff(&self.lo, &other.lo),
        );
        Self::new(
            self.lo.clone().min(other.lo.clone()),
            self.hi.max(other.hi),
            spacing,
        )
    }

    /// `{ x << k : x in self }` = `{ x * 2^k }`, under `bound`.  A count past
    /// the widest declared width leaves every value out of it, and the clamp
    /// also keeps the factor from sizing an allocation off a binary's own
    /// constant.
    pub(super) fn shl(self, k: u32, bound: &UBig) -> Self {
        if k as usize > widest_int().bit_width() {
            return Self::top(bound);
        }
        self.mul(&(UBig::ONE << k as usize), bound)
    }

    /// `{ x * c : x in self }` for `c > 0`.  Exact, or top where the product
    /// leaves `bound` or `c == 0`.
    pub(super) fn mul(self, c: &UBig, bound: &UBig) -> Self {
        let hi = &self.hi * c;
        if *c == UBig::ZERO || hi > *bound {
            return Self::top(bound);
        }
        let lo = self.lo * c;
        if lo > hi {
            return Self::empty();
        }
        let stride = self.stride * c;
        // A stride past the span names the single point `lo`, and dropping the
        // product there is what holds a chain of scalings to `bound`.
        if stride > &hi - &lo {
            return Self::new(lo.clone(), lo, UBig::ONE);
        }
        Self::new(lo, hi, stride)
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
        // Past the widest type every value shifts out; the clamp keeps the
        // shift from sizing an allocation off an attacker-chosen count.
        let k = (k as usize).min(widest_int().bit_width());
        let stride = if self.stride.trailing_zeros().is_some_and(|tz| tz >= k) {
            &self.stride >> k
        } else {
            UBig::ONE
        };
        Self::new(self.lo >> k, self.hi >> k, stride)
    }

    /// `{ x / c : x in self }` (`c == 0` treated as 1).  Floor maps the endpoints
    /// directly.  The stride survives as `stride / c` when `c` divides it, else
    /// floor breaks the progression and it widens to 1, never a stride tighter
    /// than the real spacing.
    pub(super) fn udiv(self, c: &UBig) -> Self {
        // As in `shr`: floor-mapping both endpoints would resurrect EMPTY.
        if self.is_empty() {
            return self;
        }
        let d = if *c == UBig::ZERO { &UBig::ONE } else { c };
        let stride = if &self.stride % d == UBig::ZERO {
            &self.stride / d
        } else {
            UBig::ONE
        };
        Self::new(self.lo / d, self.hi / d, stride)
    }
}

/// A monotone constant scaling of one variable operand: forward-propagating a
/// bound on the operand through it yields a bound on the result.
#[derive(Clone)]
pub(super) enum ScaleOp {
    Shl(u32),
    Shr(u32),
    Mul(UBig),
    Udiv(UBig),
}

impl ScaleOp {
    /// `bound` is the greatest value the scaled value holds.
    pub(super) fn apply(&self, iv: Interval, bound: &UBig) -> Interval {
        match self {
            ScaleOp::Shl(k) => iv.shl(*k, bound),
            ScaleOp::Shr(k) => iv.shr(*k),
            ScaleOp::Mul(c) => iv.mul(c, bound),
            ScaleOp::Udiv(c) => iv.udiv(c),
        }
    }
}
