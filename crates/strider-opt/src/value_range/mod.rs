//! Dominator-scoped per-`(value, region)` integer range analysis.
//!
//! Interval lattice with five seeds:
//!
//! - `If(IntCmp(v, const))` guards, edge-sensitive, propagated via the control
//!   dominator tree, back-propagated through an `Add(X, const)` spine, and
//!   shared by every expression of exactly `v`'s bits of one value.
//! - `KnownBits` upper bounds and strides.
//! - Guard recovery at a `Region` every predecessor of which bounds the same
//!   value.
//! - Per phi arm, the guard on the edge that arm arrives by, which is the only
//!   channel for a bound sitting below the loop header it re-enters.
//! - Forward propagation through a monotone constant scaling.
//!
//! Fail-closed: anything uncertain returns the full type range (top).
//!
//! SOUNDNESS INVARIANT: `range_of` may widen toward top but must NEVER return
//! an interval tighter than the real runtime value set.

use cranelift_entity::SecondaryMap;
use rustc_hash::FxHashMap;

use strider_ir::DominatorTree;
use strider_ir::node::{ExtendOp, NodeId, NodeKind, ValueId, ValueKind, ValueType};
use strider_ir::{IRViewer, IRWalker, IntBinaryOp, IntCmpOp};

use crate::opt::known_bits::{KnownBitsFacts, KnownBitsMap};

#[cfg(test)]
mod tests;

/// Cast hops either guard ladder ([`RangeMap::dominating_guard`],
/// [`RangeMap::edge_bound_on`]) walks before giving up.  A real index sits one
/// or two casts above its compare; the cap only stops a pathological chain from
/// making the walk the cost centre.
const MAX_GUARD_LADDER: u32 = 8;

/// Deepest chain of `(value, region)` resolutions [`RangeMap::range_of`]
/// follows.  A phi of phis costs one level each, so thousands of sequential
/// conditional assignments feeding one query would otherwise recurse past the
/// stack.  Exhaustion widens to top.
const MAX_RANGE_DEPTH: u32 = 256;

/// How much of an operand's bound survives one hop up to the value built from
/// it.  [`RangeMap::bound_carrying_operand`] yields every variant but
/// `Scaled`, which only [`RangeMap::arm_carrying_operand`] adds.
#[derive(Clone, Copy)]
enum Hop {
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
    fn lift(self, iv: Interval) -> Option<Interval> {
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
const EMPTY: Interval = Interval::new(1, 0, 1);

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
    lo: u128,
    hi: u128,
    /// At least 1.
    stride: u128,
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
    fn intersect(self, other: Self) -> Self {
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
    /// one point, where 1 is as tight.
    fn union(self, other: Self) -> Self {
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
    fn shl(self, k: u32, width_mask: u128) -> Self {
        match 1u128.checked_shl(k) {
            Some(factor) => self.mul(factor, width_mask),
            None => Self::top(width_mask),
        }
    }

    /// `{ x * c : x in self }` for `c > 0`.  Exact, or top on overflow / `c == 0`.
    fn mul(self, c: u128, width_mask: u128) -> Self {
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
    fn shr(self, k: u32) -> Self {
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
    fn udiv(self, c: u128) -> Self {
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
enum ScaleOp {
    Shl(u32),
    Shr(u32),
    Mul(u128),
    Udiv(u128),
}

impl ScaleOp {
    fn apply(self, iv: Interval, width_mask: u128) -> Interval {
        match self {
            ScaleOp::Shl(k) => iv.shl(k, width_mask),
            ScaleOp::Shr(k) => iv.shr(k),
            ScaleOp::Mul(c) => iv.mul(c, width_mask),
            ScaleOp::Udiv(c) => iv.udiv(c),
        }
    }
}

/// Cycle marking for a `(value, region)` query; absent means unvisited.
#[derive(Clone, Copy)]
enum MemoSlot {
    /// Re-entry means a dependency cycle; the back-edge resolves to top.
    InProgress,
    Done(Interval),
}

/// Per-`(value, region)` integer ranges.
pub struct RangeMap<'f> {
    function: &'f strider_ir::Function,
    doms: DominatorTree<NodeId>,
    /// Per guarded value, the interval proven AT each guard node: an `If`'s
    /// guarded successor edge, or a `Region` every one of whose predecessor
    /// edges bounds the value (a merge whose arms all constrain it).
    guards: FxHashMap<ValueId, FxHashMap<NodeId, Interval>>,
    /// `guards` on the dominator tree, so a lookup climbs only the guards
    /// dominating the query point.
    guard_forests: FxHashMap<ValueId, GuardForest>,
    /// Flow-insensitive `[0, max_value]` bounds from KnownBits.
    kb_bounds: SecondaryMap<ValueId, Option<Interval>>,
    /// Resolved intervals, with `InProgress` cutting resolution cycles.
    memo: FxHashMap<(ValueId, NodeId), MemoSlot>,
    /// Per `If`-controlled edge, what taking it proves and about which value.
    ///
    /// Distinct from `guards`, which is keyed by the node a guard holds AT and
    /// so cannot record one whose edge lands on a merge. An edge fact holds
    /// only for the value ARRIVING on that edge, which is what a phi arm is.
    edge_guards: FxHashMap<ValueId, (ValueId, Interval)>,
    /// `dominating_guard` is a pure function of `guards` and `doms`, both
    /// fixed here, and every `range_of` frame asks it at least once.
    guard_memo: std::cell::RefCell<FxHashMap<(ValueId, NodeId), Option<Interval>>>,
    /// Per `And(x, c)` output whose mask clears only bits KnownBits proves zero
    /// in `x`, that `x`: the two are the same value.
    identity_masks: SecondaryMap<ValueId, Option<ValueId>>,
    /// The guarded values whose [`BitField`] is exactly this one.
    guards_by_field: FxHashMap<BitField, Vec<ValueId>>,
    /// The distinct keys of `guards_by_field`, by root.
    fields_by_root: FxHashMap<ValueId, Vec<BitField>>,
    /// [`Self::bit_field`] answers, computed on first ask.
    field_memo: std::cell::RefCell<SecondaryMap<ValueId, Option<Option<BitField>>>>,
    /// Open `range_of` frames, against [`MAX_RANGE_DEPTH`].
    depth: u32,
}

impl<'f> RangeMap<'f> {
    /// The ranges of `function`: builds the flow-insensitive KnownBits bases,
    /// extracts per-`If` guard facts, then recovers guards at a merge every
    /// predecessor of which bounds the same value.
    ///
    /// # Errors
    ///
    /// Propagates a KnownBits failure.
    pub fn new(function: &'f strider_ir::Function) -> crate::Result<Self> {
        let known = crate::opt::known_bits::analyze(function)?;
        Ok(build(
            function,
            strider_ir::control_dominator_tree(function),
            &known,
        ))
    }

    /// Range of `value` valid within `region`.
    ///
    /// A multi-input phi unions each arm's range in that arm's predecessor
    /// region, falling back to a guard on the phi's own output if any arm is
    /// top.  Any other value intersects its dominating guards with the
    /// KnownBits base.  Unconstrained means top.
    pub fn range_of(&mut self, value: ValueId, region: NodeId) -> Interval {
        // A width past the `u128` carrier has no mask the value is inside, so
        // `Interval::top` there is not a bound. Refusing locally keeps the
        // non-bound out of `const_scale`, which would otherwise turn it into a
        // tight claim (`top(u128::MAX).shr(200)` is "exactly zero").
        if self
            .function
            .value_type_opt(value)
            .and_then(ValueType::int_mask_u128)
            .is_none()
        {
            return Interval::top(u128::MAX);
        }
        let key = (value, region);

        // A cycle, cut by returning top for the back-edge; the frame that
        // opened it still applies its dominating guards.
        match self.memo.get(&key) {
            Some(MemoSlot::Done(iv)) => return *iv,
            Some(MemoSlot::InProgress) => {
                let ty = self.function.value_type_opt(value);
                return Interval::top(type_mask_or_top(ty));
            }
            None => {}
        }

        // Budget exhausted: top, left out of the memo so a shallower query of
        // the same key still resolves it.
        if self.depth >= MAX_RANGE_DEPTH {
            let ty = self.function.value_type_opt(value);
            return Interval::top(type_mask_or_top(ty));
        }
        self.memo.insert(key, MemoSlot::InProgress);
        self.depth += 1;

        let producer = self.function.producer(value);

        // A monotone constant scaling (`idx << k`, the value a table index
        // feeds into its address) propagates only under a dominating GUARD on
        // the operand: KnownBits scales the stride but loses the upper bound
        // across the scale, so an unguarded `byte * 4` would resolve as a
        // 256-case index.
        let result = if matches!(self.function.node_kind(producer), NodeKind::Phi) {
            self.resolve_phi(producer, region)
        } else if let Some(operand) = self.identity_masks[value] {
            let leaf = self.resolve_leaf(value, region);
            self.range_of(operand, region).intersect(leaf)
        } else {
            let leaf = self.resolve_leaf(value, region);
            match self.const_scale(value) {
                Some((operand, scale)) => match self.dominating_guard(operand, region) {
                    Some(guard) => {
                        let width_mask = type_mask_or_top(self.function.value_type_opt(value));
                        let inner = self.resolve_leaf_guarded(operand, Some(guard));
                        scale.apply(inner, width_mask).intersect(leaf)
                    }
                    None => leaf,
                },
                None => leaf,
            }
        };

        self.depth -= 1;
        self.memo.insert(key, MemoSlot::Done(result));
        result
    }

    /// `region` applies only to a guard on the phi's own output; each arm is
    /// queried in its own effective region.
    fn resolve_phi(&mut self, phi_node: NodeId, region: NodeId) -> Interval {
        let data_inputs: Vec<ValueId> = self.function.phi_data_inputs(phi_node).collect();

        let Some(phi_value) = self
            .function
            .graph()
            .node_outputs(phi_node)
            .first()
            .copied()
        else {
            return Interval::top(u128::MAX);
        };
        let ty = self.function.value_type_opt(phi_value);
        let type_mask = type_mask_or_top(ty);

        if data_inputs.len() < 2 {
            return Interval::top(type_mask);
        }

        // The Region's control inputs line up 1-to-1 with the phi's data
        // inputs, so each arm is queried in its own effective region: a guard
        // holding on one incoming path must never apply to an arm bypassing it.
        let Some(joining_region) = self.find_joining_region(phi_node) else {
            return Interval::top(type_mask);
        };

        let arm_regions = self.arm_query_regions(joining_region);
        if arm_regions.len() != data_inputs.len() {
            return Interval::top(type_mask);
        }

        let mut result: Option<Interval> = None;
        for (arm_region, &arm_val) in arm_regions.iter().zip(data_inputs.iter()) {
            let arm_range = self.range_of(arm_val, *arm_region);
            if arm_range.is_top(type_mask) {
                // The union is top, but a guard on the phi's own output still
                // bounds it at the query point whichever arm the value took.
                // The fallback is a fresh top: `arm_range` may carry a stride
                // only this arm obeys.
                return self
                    .dominating_guard(phi_value, region)
                    .unwrap_or_else(|| Interval::top(type_mask));
            }
            result = Some(result.map_or(arm_range, |acc| acc.union(arm_range)));
        }

        let union = result.expect("union has >= 1 arm by the guards above");
        // A guard holding at the query point is valid for every arm.
        match self.dominating_guard(phi_value, region) {
            Some(guard) => union.intersect(guard),
            None => union,
        }
    }

    /// If `value` is a monotone constant scaling of one variable operand
    /// (`x << k`, `x * c`, `x >> k`, `x / c`), the operand and how to transform
    /// its range.  The constant must be the RHS for the non-commutative shapes.
    fn const_scale(&self, value: ValueId) -> Option<(ValueId, ScaleOp)> {
        let NodeKind::IntBinaryOp(op) = *self.function.node_kind(self.function.producer(value))
        else {
            return None;
        };
        let [lhs, rhs] = self.function.producer_inputs_exact::<2>(value).ok()?;
        let rc = self.function.int_const_u128(rhs);
        Some(match op {
            IntBinaryOp::ShiftLeft => (lhs, ScaleOp::Shl(u32::try_from(rc?).ok()?)),
            IntBinaryOp::ShiftRight => (lhs, ScaleOp::Shr(u32::try_from(rc?).ok()?)),
            // A divide by zero traps, and `eval::int_binary` refuses to fold it,
            // so the node reaches here; `udiv` would read it as a divide by one.
            IntBinaryOp::Div => match rc? {
                0 => return None,
                c => (lhs, ScaleOp::Udiv(c)),
            },
            // Mul is commutative, so the variable may sit on either side.
            IntBinaryOp::Mul => {
                let (x, c) =
                    crate::const_eval::var_and_const(self.function, self.function.producer(value))?;
                (x, ScaleOp::Mul(c))
            }
            _ => return None,
        })
    }

    /// Intersection of every guard on `value` whose `guard_node` dominates
    /// `region`, or `None` when none apply.
    ///
    /// Also carries up a guard recorded on a value `value` is built from by a
    /// chain of [`Self::bound_carrying_operand`] hops: a compare on `al` bounds
    /// the `movzbl %al` an addressing mode scales, and without the walk the
    /// index falls back to the KnownBits width bound and reads the whole table
    /// domain.
    fn dominating_guard(&self, value: ValueId, region: NodeId) -> Option<Interval> {
        if let Some(hit) = self.guard_memo.borrow().get(&(value, region)) {
            return *hit;
        }
        let mut verdict = self.bound_at(value, region);

        // `hi` survives every hop, so one accumulated mask decides all the
        // `Truncate`s at once.
        //
        // Not `Hop::lift`: that maps ONE hop's interval, while this folds a
        // ladder of them into a single mask and flag before lifting once. The
        // match below is exhaustive on purpose -- a new `Hop` has to decide
        // whether it folds, and a catch-all would silently treat it as
        // `SameValue`.
        let mut cur = value;
        let mut trunc_mask = u128::MAX;
        let mut upper_only = false;
        for _ in 0..MAX_GUARD_LADDER {
            let Some((operand, hop)) = self.bound_carrying_operand(cur) else {
                break;
            };
            match hop {
                Hop::SameValue => {}
                Hop::SameValueBelow(mask) => trunc_mask = trunc_mask.min(mask),
                Hop::UpperBound => upper_only = true,
                // One accumulated mask cannot express a scaling, and
                // `bound_carrying_operand` never yields one.
                Hop::Scaled(..) => break,
            }
            if let Some(below) = self.bound_at(operand, region)
                && below.hi <= trunc_mask
            {
                let lifted = if upper_only {
                    Interval::dense(0, below.hi)
                } else {
                    below
                };
                verdict = Some(verdict.map_or(lifted, |v| v.intersect(lifted)));
            }
            cur = operand;
        }

        self.guard_memo
            .borrow_mut()
            .insert((value, region), verdict);
        verdict
    }

    /// What holds on `value` itself at `region`: the guards recorded on it and
    /// on every other value whose bits enclose its own, met with what its
    /// incoming edges prove if it is a phi.
    ///
    /// MIPS64 guards `w >> 26` with `srl` and indexes by an `ext` of the same
    /// six bits, `(sext(w) << 32) >> 58`: two nodes, one value.  A guard on
    /// wider bits bounds the field inside them: `w < 20` puts `(w >> 4) & 7`
    /// at most 1.
    fn bound_at(&self, value: ValueId, region: NodeId) -> Option<Interval> {
        let own = match (
            self.guard_at(value, region),
            self.phi_arm_bound(value, region),
        ) {
            (Some(a), Some(b)) => Some(a.intersect(b)),
            (hit, None) | (None, hit) => hit,
        };
        let Some(field) = self.bit_field(value).filter(|f| f.exact) else {
            return own;
        };
        let mut met = own;
        for &of in self.root_fields(field.root) {
            if !of.encloses(field) {
                continue;
            }
            for &guarded in &self.guards_by_field[&of] {
                if guarded != value
                    && let Some(iv) = self.guard_at(guarded, region)
                    && let Some(bound) = of.project(field, iv)
                {
                    met = Some(met.map_or(bound, |m| m.intersect(bound)));
                }
            }
        }
        met
    }

    /// Whether `iv`, `value`'s range at `region`, is no tighter than its
    /// KnownBits bound while a guard dominating `region` bounds bits of the
    /// same root that `value` overlaps, but in a way
    /// [`BitField::project`] does not carry to `value`.
    ///
    /// That guard may be the one sizing a table `value` indexes, in a shape
    /// this module does not read, and the KnownBits bound of a bit field is
    /// the whole field: enumerating it reads past the table into whatever
    /// follows, such as the next switch's table.
    pub(crate) fn guard_left_unconnected(
        &self,
        value: ValueId,
        region: NodeId,
        iv: Interval,
    ) -> bool {
        if iv != self.resolve_leaf_guarded(value, None) {
            return false;
        }
        let Some(field) = self.bit_field(value) else {
            return false;
        };
        self.root_fields(field.root).iter().any(|&of| {
            of.overlaps(field)
                && self.guards_by_field[&of].iter().any(|&guarded| {
                    guarded != value
                        && self
                            .guard_at(guarded, region)
                            .is_some_and(|iv| of.project(field, iv).is_none())
                })
        })
    }

    /// The distinct fields of `root` some guarded value is.
    fn root_fields(&self, root: ValueId) -> &[BitField] {
        self.fields_by_root.get(&root).map_or(&[], Vec::as_slice)
    }

    /// `value` as bits of the value it is extracted from; see [`BitField`].
    fn bit_field(&self, value: ValueId) -> Option<BitField> {
        if let Some(hit) = self.field_memo.borrow()[value] {
            return hit;
        }
        let field = bit_field(self.function, &self.identity_masks, value);
        self.field_memo.borrow_mut()[value] = Some(field);
        field
    }

    /// The union over a phi's arms of what reaching it by that arm's edge
    /// proves: a constant arm is its own value, and a guarded one is the
    /// interval its `If` edge establishes on exactly that arm value.  `None`
    /// unless every arm is one of the two.
    ///
    /// An edge fact is not a node fact: the merge the edge lands on is reached
    /// by the other predecessors too, so the interval bounds only the value
    /// arriving along that one edge, which is the arm.  A loop back edge is the
    /// case the mask alone loses, its guard sitting below the header it
    /// re-enters.
    fn phi_arm_bound(&self, value: ValueId, region: NodeId) -> Option<Interval> {
        let f = self.function;
        let phi_node = f.producer(value);
        if !matches!(f.node_kind(phi_node), NodeKind::Phi) {
            return None;
        }
        let joining = self.find_joining_region(phi_node)?;
        if !self.dominates(joining, region) {
            return None;
        }
        let data: Vec<ValueId> = f.phi_data_inputs(phi_node).collect();
        let ctrl: Vec<ValueId> = f
            .graph()
            .node_inputs(joining)
            .iter()
            .filter(|&v| f.value_kind(v).is_control())
            .collect();
        if ctrl.len() != data.len() || data.len() < 2 {
            return None;
        }
        ctrl.iter()
            .zip(data.iter())
            .map(|(&edge, &arm)| match f.int_const_u128(arm) {
                Some(k) => Some(Interval::dense(k, k)),
                None => self.edge_bound_on(edge, arm),
            })
            .collect::<Option<Vec<_>>>()?
            .into_iter()
            .reduce(Interval::union)
    }

    /// What taking `edge` proves about the value `arm` arriving on it, mapped up
    /// the hops between the compared value and the arm: the compare is on the
    /// register, the arm on a widened, masked or already-scaled copy of it.
    ///
    /// The hops are collected downward and replayed upward, so each one sees
    /// the interval its own operand really carries.
    fn edge_bound_on(&self, edge: ValueId, arm: ValueId) -> Option<Interval> {
        let (guarded, iv) = *self.edge_guards.get(&edge)?;
        let mut hops: Vec<Hop> = Vec::new();
        let mut cur = arm;
        for _ in 0..MAX_GUARD_LADDER {
            if cur == guarded {
                break;
            }
            let (operand, hop) = self.arm_carrying_operand(cur)?;
            hops.push(hop);
            cur = operand;
        }
        if cur == guarded {
            return hops.iter().rev().try_fold(iv, |acc, hop| hop.lift(acc));
        }
        None
    }

    /// [`Self::bound_carrying_operand`] plus the monotone constant scalings, which
    /// a bound survives as a mapped interval rather than unchanged.
    fn arm_carrying_operand(&self, value: ValueId) -> Option<(ValueId, Hop)> {
        if let Some(hit) = self.bound_carrying_operand(value) {
            return Some(hit);
        }
        let (operand, scale) = self.const_scale(value)?;
        let mask = type_mask_or_top(self.function.value_type_opt(value));
        Some((operand, Hop::Scaled(scale, mask)))
    }

    /// `x` when `value` is an `And(x, c)` whose mask clears only bits known zero
    /// in `x`, i.e. the same value as `x`.
    pub(crate) fn identity_mask_operand(&self, value: ValueId) -> Option<ValueId> {
        self.identity_masks[value]
    }

    /// Reflexive: a node dominates itself.
    fn dominates(&self, node: NodeId, region: NodeId) -> bool {
        self.doms.dominates(node, region)
    }

    /// The guards recorded on `value` itself whose node dominates `region`,
    /// met from `region` up towards the entry.
    fn guard_at(&self, value: ValueId, region: NodeId) -> Option<Interval> {
        #[cfg(test)]
        GUARD_SCANS.with(|c| c.set(c.get() + 1));
        let Some(span) = self.doms.span(region) else {
            // Off the dominator tree, `dominates` still holds reflexively.
            return self.guards.get(&value)?.get(&region).copied();
        };
        self.guard_forests.get(&value)?.meet_dominating(span)
    }

    /// The operand whose bound `value` inherits, and on what terms.  `None`
    /// when no hop is sound, which ends the walk.
    fn bound_carrying_operand(&self, value: ValueId) -> Option<(ValueId, Hop)> {
        let f = self.function;
        match *f.node_kind(f.producer(value)) {
            // Zero extension is the identity on the unsigned value.
            NodeKind::Extend(ExtendOp::ZeroExtend) => {
                let [x] = f.producer_inputs_exact::<1>(value).ok()?;
                Some((x, Hop::SameValue))
            }
            // Truncation is the identity only below the narrower type's mask.
            NodeKind::Truncate => {
                let [x] = f.producer_inputs_exact::<1>(value).ok()?;
                let mask = f.value_type_opt(value)?.int_mask_u128()?;
                Some((x, Hop::SameValueBelow(mask)))
            }
            // Masking only clears bits, so `x & c <= x` carries the upper bound
            // while the low end and the stride do not survive.
            NodeKind::IntBinaryOp(IntBinaryOp::And) => {
                let (x, _) = crate::const_eval::var_and_const(f, f.producer(value))?;
                Some((x, Hop::UpperBound))
            }
            _ => None,
        }
    }

    /// The `Region` whose PhiToken is this Phi's slot-0 input.
    fn find_joining_region(&self, phi_node: NodeId) -> Option<NodeId> {
        let f = self.function;
        let phi_token_val = f.nth_input(phi_node, 0)?;
        if f.value_kind(phi_token_val) != ValueKind::PhiToken {
            return None;
        }
        let region_node = f.producer(phi_token_val);
        if matches!(f.node_kind(region_node), NodeKind::Region) {
            Some(region_node)
        } else {
            None
        }
    }

    /// Per control input of `joining_region`, the effective query region for
    /// that arm: the joining region itself for an arm arriving on an If's TRUE
    /// edge, else the arm's own source Region.
    fn arm_query_regions(&self, joining_region: NodeId) -> Vec<NodeId> {
        let g = self.function.graph();
        // Every `Region` input is a Control edge per the node signature.
        g.node_inputs(joining_region)
            .iter()
            .filter(|&v| g.value_kind(v).is_control())
            .map(|ctrl_val| {
                let producer = g.producer(ctrl_val);
                // An If's outputs are [true_ctrl, false_ctrl].
                if matches!(g.node_kind(producer), NodeKind::If) {
                    let if_outputs = g.node_outputs(producer);
                    if !if_outputs.is_empty() && if_outputs[0] == ctrl_val {
                        return joining_region;
                    }
                }
                self.ctrl_source_region(ctrl_val)
            })
            .collect()
    }

    /// Walks control producers back past `If` / `Call` / `CallOther` to the
    /// node the control originates at.
    fn ctrl_source_region(&self, ctrl_val: ValueId) -> NodeId {
        let f = self.function;
        let mut curr = f.producer(ctrl_val);
        loop {
            match f.node_kind(curr) {
                NodeKind::If | NodeKind::Call { .. } | NodeKind::CallOther { .. } => {
                    // The controlling predecessor is the first input.
                    if let Some(pred_ctrl) = f.nth_input(curr, 0) {
                        curr = f.producer(pred_ctrl);
                    } else {
                        return curr;
                    }
                }
                _ => return curr,
            }
        }
    }

    /// Intersects `value`'s dominating guards with its KnownBits base.
    fn resolve_leaf(&self, value: ValueId, region: NodeId) -> Interval {
        self.resolve_leaf_guarded(value, self.dominating_guard(value, region))
    }

    /// [`Self::resolve_leaf`] against an already-computed dominating guard.
    fn resolve_leaf_guarded(&self, value: ValueId, guard: Option<Interval>) -> Interval {
        let type_mask = type_mask_or_top(self.function.value_type_opt(value));
        guard
            .into_iter()
            .chain(self.kb_bounds[value])
            .reduce(Interval::intersect)
            .unwrap_or_else(|| Interval::top(type_mask))
    }
}

#[cfg(test)]
thread_local! {
    /// [`RangeMap::guard_at`] lookups, one per guard scan of a value.
    pub(crate) static GUARD_SCANS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    /// Guards met by a lookup, one per guard dominating the query point: the
    /// cost a query pays, which must track neither the chain's depth nor the
    /// value's guard count.
    pub(crate) static GUARD_PROBES: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// `u128::MAX` (unconstrained top) when the value carries no typed edge, for a
/// non-integer type, and for a width past the `u128` carrier, where no `u128`
/// is a bound the value is inside. Every consumer that needs a real bound gates
/// on [`ValueType::int_mask_u128`], which answers `None` in all three cases.
fn type_mask_or_top(ty: Option<ValueType>) -> u128 {
    ty.and_then(ValueType::int_mask_u128).unwrap_or(u128::MAX)
}

/// For `value = Add(X, IntConst(c))`, the bound `X` inherits from a guard on
/// `X + c`: `interval` shifted by `-c` mod the operand width.  `None` if the
/// value is not such an add, or the shift wraps (a straddling interval has no
/// single `[lo, hi]` form).
fn add_operand_shifted_interval(
    function: &strider_ir::Function,
    value: ValueId,
    interval: Interval,
) -> Option<(ValueId, Interval)> {
    if !matches!(
        function.kind_of_value(value),
        NodeKind::IntBinaryOp(IntBinaryOp::Add)
    ) {
        return None;
    }
    let (operand, c) = crate::const_eval::var_and_const(function, function.producer(value))?;
    // Past 128 bits there is no `u128` modulus, so the shift would wrap mod
    // 2^128 instead of the type's and the `lo <= hi` test would accept a
    // TIGHTER-than-real interval, the one thing this module must never return.
    let mask = function.value_type_opt(operand)?.int_mask_u128()?;
    let lo = interval.lo.wrapping_sub(c) & mask;
    let hi = interval.hi.wrapping_sub(c) & mask;
    (lo <= hi).then_some((operand, Interval::dense(lo, hi)))
}

/// KnownBits proves `value` is always non-negative read as signed.
fn is_sign_bit_known_zero(
    function: &strider_ir::Function,
    value: ValueId,
    known: &KnownBitsMap,
) -> bool {
    let Some(ty) = function.value_type_opt(value) else {
        return false;
    };
    let Some(type_mask) = ty.int_mask_u128() else {
        return false;
    };
    let sign_bit = (type_mask >> 1) + 1;
    known[value].zeros & sign_bit != 0
}

/// One value's guard nodes in dominator-tree preorder.
struct GuardForest {
    entries: Vec<GuardEntry>,
    /// `max_post[l][i]`: the largest `post` among `entries[i..i + 2^l]`.
    max_post: Vec<Vec<u32>>,
}

struct GuardEntry {
    pre: u32,
    post: u32,
    interval: Interval,
    /// The nearest entry whose node dominates this one's.
    parent: Option<usize>,
}

impl GuardForest {
    fn new(doms: &DominatorTree<NodeId>, by_node: &FxHashMap<NodeId, Interval>) -> Self {
        let mut entries: Vec<GuardEntry> = by_node
            .iter()
            .filter_map(|(&node, &interval)| {
                let (pre, post) = doms.span(node)?;
                Some(GuardEntry {
                    pre,
                    post,
                    interval,
                    parent: None,
                })
            })
            .collect();
        entries.sort_unstable_by_key(|e| e.pre);
        let mut open: Vec<usize> = Vec::new();
        for i in 0..entries.len() {
            while open
                .last()
                .is_some_and(|&top| entries[top].post < entries[i].post)
            {
                open.pop();
            }
            entries[i].parent = open.last().copied();
            open.push(i);
        }
        let mut max_post = vec![entries.iter().map(|e| e.post).collect::<Vec<u32>>()];
        while 2usize << (max_post.len() - 1) <= entries.len() {
            let below = max_post.last().expect("level 0 exists");
            let half = 1usize << (max_post.len() - 1);
            let level = (0..=entries.len() - 2 * half)
                .map(|i| below[i].max(below[i + half]))
                .collect();
            max_post.push(level);
        }
        Self { entries, max_post }
    }

    /// The guards whose nodes enclose `span`, met nearest first.
    fn meet_dominating(&self, (pre, post): (u32, u32)) -> Option<Interval> {
        // Every entry up to `end` starts at or before `pre`, so it encloses the
        // query iff it has not finished before `post`; the last such is the
        // nearest dominating guard.
        let mut end = self.entries.partition_point(|e| e.pre <= pre);
        for level in (0..self.max_post.len()).rev() {
            let width = 1usize << level;
            if end >= width && self.max_post[level][end - width] < post {
                end -= width;
            }
        }
        let mut at = end.checked_sub(1)?;
        let mut met = self.entries[at].interval;
        loop {
            #[cfg(test)]
            GUARD_PROBES.with(|c| c.set(c.get() + 1));
            let Some(parent) = self.entries[at].parent else {
                return Some(met);
            };
            at = parent;
            met = met.intersect(self.entries[at].interval);
        }
    }
}

/// Records `interval` as proven at `node`, intersecting with any bound already
/// recorded there.
fn add_guard(
    guards: &mut FxHashMap<ValueId, FxHashMap<NodeId, Interval>>,
    value: ValueId,
    node: NodeId,
    interval: Interval,
) {
    guards
        .entry(value)
        .or_default()
        .entry(node)
        .and_modify(|iv| *iv = iv.intersect(interval))
        .or_insert(interval);
}

fn build<'f>(
    function: &'f strider_ir::Function,
    doms: DominatorTree<NodeId>,
    known: &KnownBitsMap,
) -> RangeMap<'f> {
    let mut kb_bounds: SecondaryMap<ValueId, Option<Interval>> = SecondaryMap::new();
    let mut identity_masks: SecondaryMap<ValueId, Option<ValueId>> = SecondaryMap::new();
    for node in function.walk() {
        if let Some((value, operand)) = identity_mask(function, node, known) {
            identity_masks[value] = Some(operand);
        }
        for &value_id in function.node_outputs(node) {
            let kb: KnownBitsFacts = known[value_id];
            if kb.ones == 0 && kb.zeros == 0 {
                continue;
            }
            let Some(ty) = function.value_type_opt(value_id) else {
                continue;
            };
            let Some(type_mask) = ty.int_mask_u128() else {
                continue;
            };
            let max_val = kb.max_value(type_mask);
            // `k` trailing known-zero bits means a multiple of `2^k`, capped to
            // the range so the stride stays a valid divisor.
            let tz = kb.zeros.trailing_ones().min(127);
            let stride = (1u128 << tz).min(max_val.max(1));
            if max_val < type_mask || stride > 1 {
                kb_bounds[value_id] = Some(Interval::new(0, max_val, stride));
            }
        }
    }

    let mut guards: FxHashMap<ValueId, FxHashMap<NodeId, Interval>> = FxHashMap::default();
    let mut edge_guards: FxHashMap<ValueId, (ValueId, Interval)> = FxHashMap::default();

    for if_node in function.walk_kind(|k| matches!(k, NodeKind::If)) {
        let if_outputs = function.node_outputs(if_node);
        if if_outputs.len() < 2 {
            continue;
        }
        let true_ctrl = if_outputs[0];
        let false_ctrl = if_outputs[1];

        let cond_value = function.if_cond(if_node);

        for (edge_ctrl, edge_taken) in [(true_ctrl, true), (false_ctrl, false)] {
            let Some((guarded_value, guard_interval)) =
                guard_from_compare(function, cond_value, edge_taken, known)
            else {
                continue;
            };

            edge_guards.insert(edge_ctrl, (guarded_value, guard_interval));

            // Soundness gate: attach the guard only where the consumer is
            // reached EXCLUSIVELY via this edge.  A `Region` still consuming an
            // edge is a genuine merge, so the guard does not hold for its other
            // predecessors.
            let Some(consumer) = single_control_consumer(function, edge_ctrl) else {
                continue;
            };
            if matches!(function.node_kind(consumer), NodeKind::Region) {
                continue;
            }

            add_guard(&mut guards, guarded_value, consumer, guard_interval);

            // Back-propagate through `Add(X, const)`: a guard on `idx - K`
            // gives `idx` the shifted bound.
            // Bounded like every other walk here: an `Add` spine is linear in
            // the function.
            const MAX_ADD_BACKPROP: usize = 64;
            let mut cur = guarded_value;
            let mut iv = guard_interval;
            let mut steps = 0usize;
            while let Some((operand, shifted)) = add_operand_shifted_interval(function, cur, iv) {
                if steps == MAX_ADD_BACKPROP {
                    break;
                }
                steps += 1;
                add_guard(&mut guards, operand, consumer, shifted);
                cur = operand;
                iv = shifted;
            }
        }
    }

    // A value bounded on EVERY control-predecessor edge of a merge is bounded
    // at the merge by the union, though no single guard's edge dominates past
    // it.  One level: a predecessor that is not an `If` bounds nothing and
    // blocks the join.
    for region in function.walk_kind(|k| matches!(k, NodeKind::Region)) {
        let pred_edges: Vec<ValueId> = function
            .graph()
            .node_inputs(region)
            .iter()
            .filter(|&v| function.value_kind(v).is_control())
            .collect();
        if pred_edges.len() < 2 {
            continue;
        }
        let Some(per_edge) = pred_edges
            .iter()
            .map(|&e| edge_guard(function, e, known))
            .collect::<Option<Vec<(ValueId, Interval)>>>()
        else {
            continue;
        };
        let (v0, _) = per_edge[0];
        if !per_edge.iter().all(|(v, _)| *v == v0) {
            continue;
        }
        let union = per_edge
            .iter()
            .map(|(_, iv)| *iv)
            .reduce(Interval::union)
            .expect("pred_edges has >= 2 entries");
        add_guard(&mut guards, v0, region, union);
    }

    let guard_forests = guards
        .iter()
        .map(|(&value, by_node)| (value, GuardForest::new(&doms, by_node)))
        .collect();
    let mut guards_by_field: FxHashMap<BitField, Vec<ValueId>> = FxHashMap::default();
    let mut fields_by_root: FxHashMap<ValueId, Vec<BitField>> = FxHashMap::default();
    for &value in guards.keys() {
        if let Some(field) = bit_field(function, &identity_masks, value) {
            let guarded = guards_by_field.entry(field).or_default();
            if guarded.is_empty() {
                fields_by_root.entry(field.root).or_default().push(field);
            }
            guarded.push(value);
        }
    }
    RangeMap {
        function,
        doms,
        guards,
        guard_forests,
        edge_guards,
        kb_bounds,
        memo: FxHashMap::default(),
        guard_memo: std::cell::RefCell::new(FxHashMap::default()),
        identity_masks,
        guards_by_field,
        fields_by_root,
        field_memo: std::cell::RefCell::new(SecondaryMap::new()),
        depth: 0,
    }
}

/// `(output, x)` for an `And(x, c)` node whose mask clears only bits known zero
/// in `x`: `slwi` / `sldi` lift to `And(ShiftLeft(idx, k), ~(2^k - 1))`.
fn identity_mask(
    function: &strider_ir::Function,
    node: NodeId,
    known: &KnownBitsMap,
) -> Option<(ValueId, ValueId)> {
    if !matches!(
        function.node_kind(node),
        NodeKind::IntBinaryOp(IntBinaryOp::And)
    ) {
        return None;
    }
    let &[value] = function.node_outputs(node) else {
        return None;
    };
    let (operand, mask) = crate::const_eval::var_and_const(function, node)?;
    let type_mask = function.value_type_opt(value)?.int_mask_u128()?;
    let cleared = !mask & type_mask;
    (cleared & !known[operand].zeros == 0).then_some((value, operand))
}

/// Producer steps [`bit_field`] takes below a value; past them the value
/// reached is the root.
const MAX_FIELD_STEPS: u32 = 16;

/// A value read as a run of bits of `root`: its bits `shift..shift + width`
/// are `root`'s bits `lo..lo + width`, and its bits below `shift` are zero.
/// When `exact` the bits above the run are zero too, so two values with one
/// exact field are one unsigned value; otherwise they are other bits of
/// `root`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
struct BitField {
    root: ValueId,
    lo: u32,
    width: u32,
    shift: u32,
    exact: bool,
}

impl BitField {
    /// Whether the two may share a bit of `root`.
    fn overlaps(self, other: BitField) -> bool {
        self.root == other.root
            && (!self.exact
                || !other.exact
                || (self.lo < other.lo + other.width && other.lo < self.lo + self.width))
    }

    /// Whether `inner` is exact and its run lies inside this one's, which is
    /// what lets [`Self::project`] bound it.
    fn encloses(self, inner: BitField) -> bool {
        self.root == inner.root
            && inner.exact
            && self.lo <= inner.lo
            && inner.lo + inner.width <= self.lo + self.width
    }

    /// What `iv`, an interval on a value with this field, proves about a value
    /// with the field `inner`, or `None` unless it [`encloses`](Self::encloses)
    /// `inner` and `iv`'s low end carries.
    ///
    /// The same exact field is the same value, so `iv` itself.  Otherwise the
    /// run's bits read as a number are at most `iv.hi >> shift`, whatever lies
    /// above them, and `inner`'s are that number's bits from `inner.lo - lo`
    /// on: at most `(iv.hi >> shift) >> (inner.lo - lo)`, and at most
    /// `inner`'s own width.  Only the top of an exact run is monotone in the
    /// value, so only there does `iv.lo` bound `inner` from below; anywhere
    /// else a low end above zero is refused: dropping it would enumerate
    /// values below it that the field never takes (`x` in `[10, 17]` puts
    /// `(x >> 1) & 7` in `{5, 6, 7, 0}`, not `[0, 7]`).
    fn project(self, inner: BitField, iv: Interval) -> Option<Interval> {
        if self == inner {
            return Some(iv);
        }
        if !self.encloses(inner) || inner.shift + inner.width > 128 {
            return None;
        }
        let digits = |v: u128| {
            v.checked_shr(self.shift)
                .and_then(|run| run.checked_shr(inner.lo - self.lo))
                .unwrap_or(0)
        };
        let at_top = self.exact && inner.lo + inner.width == self.lo + self.width;
        let lo = match iv.lo {
            0 => 0,
            lo if at_top => digits(lo),
            _ => return None,
        };
        let width_max = u128::MAX >> (128 - inner.width);
        Some(Interval::new(
            lo << inner.shift,
            digits(iv.hi).min(width_max) << inner.shift,
            1u128 << inner.shift,
        ))
    }
}

/// One step of [`bit_field`]'s walk: how the value is built from `operand`.
#[derive(Clone, Copy)]
enum FieldOp {
    /// The same unsigned value.
    Same,
    SignExtend,
    Truncate,
    ShiftLeft(u32),
    ShiftRight(u32),
    /// `& (2^m - 1)`.
    LowMask(u32),
}

impl FieldOp {
    /// The field of a value of width `w` built by this op from one whose field
    /// is `f` and whose width is `w_in`; `None` when no bit of `root` is left.
    fn apply(self, f: BitField, w: u32, w_in: u32) -> Option<BitField> {
        Some(match self {
            FieldOp::Same => f,
            FieldOp::SignExtend => BitField {
                exact: f.exact && f.shift + f.width < w_in,
                ..f
            },
            FieldOp::Truncate => {
                let room = w.checked_sub(f.shift).filter(|&r| r > 0)?;
                BitField {
                    width: f.width.min(room),
                    exact: f.exact || f.width >= room,
                    ..f
                }
            }
            FieldOp::ShiftLeft(k) => {
                let shift = f.shift.checked_add(k).filter(|&s| s < w)?;
                BitField {
                    width: f.width.min(w - shift),
                    shift,
                    exact: f.exact || shift + f.width >= w,
                    ..f
                }
            }
            FieldOp::ShiftRight(k) if k <= f.shift => BitField {
                shift: f.shift - k,
                ..f
            },
            FieldOp::ShiftRight(k) => {
                let dropped = k - f.shift;
                let width = f.width.checked_sub(dropped).filter(|&w| w > 0)?;
                BitField {
                    lo: f.lo + dropped,
                    width,
                    shift: 0,
                    ..f
                }
            }
            FieldOp::LowMask(m) => {
                let room = m.checked_sub(f.shift).filter(|&r| r > 0)?;
                BitField {
                    width: f.width.min(room),
                    exact: f.exact || f.width >= room,
                    ..f
                }
            }
        })
    }
}

/// How `value` is built from one operand by an op [`bit_field`] reads.
fn field_step(
    function: &strider_ir::Function,
    identity_masks: &SecondaryMap<ValueId, Option<ValueId>>,
    value: ValueId,
) -> Option<(ValueId, FieldOp)> {
    let f = function;
    let shift_by = || -> Option<(ValueId, u32)> {
        let [x, k] = f.producer_inputs_exact::<2>(value).ok()?;
        Some((x, u32::try_from(f.int_const_u128(k)?).ok()?))
    };
    match *f.node_kind(f.producer(value)) {
        NodeKind::Extend(ExtendOp::ZeroExtend) => {
            Some((f.int_inputs(value).next()?, FieldOp::Same))
        }
        NodeKind::Extend(ExtendOp::SignExtend) => {
            Some((f.int_inputs(value).next()?, FieldOp::SignExtend))
        }
        NodeKind::Truncate => Some((f.int_inputs(value).next()?, FieldOp::Truncate)),
        NodeKind::IntBinaryOp(IntBinaryOp::ShiftLeft) => {
            shift_by().map(|(x, k)| (x, FieldOp::ShiftLeft(k)))
        }
        NodeKind::IntBinaryOp(IntBinaryOp::ShiftRight) => {
            shift_by().map(|(x, k)| (x, FieldOp::ShiftRight(k)))
        }
        NodeKind::IntBinaryOp(IntBinaryOp::Mul) => {
            let (x, c) = crate::const_eval::var_and_const(f, f.producer(value))?;
            c.is_power_of_two()
                .then(|| (x, FieldOp::ShiftLeft(c.trailing_zeros())))
        }
        NodeKind::IntBinaryOp(IntBinaryOp::And) => {
            if let Some(x) = identity_masks[value] {
                return Some((x, FieldOp::Same));
            }
            let (x, c) = crate::const_eval::var_and_const(f, f.producer(value))?;
            (c != 0 && c & c.wrapping_add(1) == 0)
                .then(|| (x, FieldOp::LowMask(128 - c.leading_zeros())))
        }
        _ => None,
    }
}

/// `value` as a [`BitField`] of the deepest value the shifts, masks and casts
/// below it reach; `value` itself, all its bits, when they leave nothing exact
/// to compare.  `None` for a non-integer.
fn bit_field(
    function: &strider_ir::Function,
    identity_masks: &SecondaryMap<ValueId, Option<ValueId>>,
    value: ValueId,
) -> Option<BitField> {
    let width = |v: ValueId| {
        function
            .value_type_opt(v)
            .filter(|t| t.is_integer())
            .and_then(|t| u32::try_from(t.bit_width()).ok())
    };
    let whole = |root: ValueId| {
        Some(BitField {
            root,
            lo: 0,
            width: width(root)?,
            shift: 0,
            exact: true,
        })
    };
    let mut steps: Vec<(ValueId, ValueId, FieldOp)> = Vec::new();
    let mut base = value;
    for _ in 0..MAX_FIELD_STEPS {
        let Some((operand, op)) = field_step(function, identity_masks, base) else {
            break;
        };
        steps.push((base, operand, op));
        base = operand;
    }
    let mut field = whole(base)?;
    for &(out, operand, op) in steps.iter().rev() {
        match op.apply(field, width(out)?, width(operand)?) {
            Some(next) => field = next,
            None => return whole(value),
        }
    }
    Some(field)
}

/// The `(value, interval)` an `If`-controlled edge establishes on the value its
/// condition bounds, or `None` when the edge's producer is not an `If` or its
/// condition upper-bounds nothing on the taken side.
fn edge_guard(
    function: &strider_ir::Function,
    ctrl_val: ValueId,
    known: &KnownBitsMap,
) -> Option<(ValueId, Interval)> {
    let producer = function.producer(ctrl_val);
    if !matches!(function.node_kind(producer), NodeKind::If) {
        return None;
    }
    // An `If`'s outputs are `[true_ctrl, false_ctrl]`; without both, "not the
    // true edge" would not mean the false edge.
    let outputs = function.node_outputs(producer);
    if outputs.len() < 2 {
        return None;
    }
    let edge_taken = outputs.first() == Some(&ctrl_val);
    let cond = function.if_cond(producer);
    guard_from_compare(function, cond, edge_taken, known)
}

/// The `(guarded_value, interval)` a bare `Less`/`Sless` condition constrains
/// on one edge, with `edge_taken` selecting true successor (condition holds)
/// or false (its negation).  `Sless` is gated on KnownBits sign-bit = 0.
///
/// Only upper-bounded intervals matter for table sizing, so a purely
/// lower-bounded constraint like `v >= N` returns `None`.
fn guard_from_compare(
    function: &strider_ir::Function,
    cmp_value: ValueId,
    edge_taken: bool,
    known: &KnownBitsMap,
) -> Option<(ValueId, Interval)> {
    let NodeKind::IntCmpOp(op @ (IntCmpOp::Less | IntCmpOp::Sless)) =
        *function.kind_of_value(cmp_value)
    else {
        return None;
    };
    let [lhs, rhs] = function.producer_inputs_exact::<2>(cmp_value).ok()?;

    // Const on RHS means `v < N`; const on LHS means `N < v`.
    let (guarded, n, const_on_rhs) =
        match (function.int_const_u128(lhs), function.int_const_u128(rhs)) {
            // Nothing to bound.
            (Some(_), Some(_)) | (None, None) => return None,
            (None, Some(n)) => (lhs, n, true),
            (Some(n), None) => (rhs, n, false),
        };

    let ty = function.value_type_opt(guarded)?;
    if !ty.is_integer() || ty == ValueType::I1 {
        return None;
    }
    if op == IntCmpOp::Sless && !is_sign_bit_known_zero(function, guarded, known) {
        return None;
    }
    // `None` past 128 bits, where no `u128` is a bound the value is inside.
    let type_mask = ty.int_mask_u128()?;
    // `n` is raw bits, read below as an unsigned endpoint. A NEGATIVE `Sless`
    // constant against a known-non-negative value carries no bound at all: the
    // compare is decided, and reading its two's-complement bits unsigned would
    // yield the whole type as a "bound" instead.
    if op == IntCmpOp::Sless && n & !(type_mask >> 1) != 0 {
        return None;
    }

    match (const_on_rhs, edge_taken) {
        (true, true) => {
            // N == 0 is an impossible unsigned guard.
            if n == 0 {
                return None;
            }
            Some((
                guarded,
                Interval::dense(0, n.saturating_sub(1).min(type_mask)),
            ))
        }
        (false, false) => Some((guarded, Interval::dense(0, n.min(type_mask)))),
        (true, false) | (false, true) => None,
    }
}

/// The one sink of a control edge.  `None` for a dead edge, and `None` if the
/// edge somehow has two: a guard attached to an arbitrary one of them would
/// hold on a path that never passed the compare, which is the unsound
/// direction, so this fails closed rather than taking the first.
fn single_control_consumer(function: &strider_ir::Function, ctrl_val: ValueId) -> Option<NodeId> {
    let mut uses = function
        .graph()
        .value_uses(ctrl_val)
        .map(|(consumer, _slot)| consumer);
    let only = uses.next()?;
    uses.next().is_none().then_some(only)
}
