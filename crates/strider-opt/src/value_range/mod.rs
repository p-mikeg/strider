//! Dominator-scoped per-`(value, region)` integer range analysis.
//!
//! Minimal interval lattice seeded from `If(IntCmp(v, const))` guards
//! (edge-sensitive, propagated via the control dominator tree) and from
//! `KnownBits` upper bounds.  Fail-closed: anything uncertain returns the
//! full type range (top).
//!
//! SOUNDNESS INVARIANT: `range_of` may widen toward top but must NEVER return
//! an interval tighter than the real runtime value set.

use cranelift_entity::SecondaryMap;
use rustc_hash::FxHashMap;

use petgraph::algo::dominators::Dominators;
use strider_ir::node::{NodeId, NodeKind, ValueId, ValueKind, ValueType};
use strider_ir::{IRViewer, IRWalker, IntBinaryOp, IntCmpOp, dominates};

use crate::opt::known_bits::{KnownBitsFacts, KnownBitsMap};

#[cfg(test)]
mod tests;

fn gcd(mut a: u128, mut b: u128) -> u128 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a.max(1)
}

/// The inclusive value set `{ lo, lo+stride, lo+2*stride, ... hi }`.
///
/// `lo`/`hi` are unsigned regardless of signedness: guard extraction gates
/// `Sless` on KnownBits sign-bit = 0, so the interval is non-negative.
///
/// `stride` is a must-congruence from KnownBits, always a sound divisor of the
/// real spacing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Interval {
    pub lo: u128,
    pub hi: u128,
    pub stride: u128,
}

impl Interval {
    pub fn top(width_mask: u128) -> Self {
        Self {
            lo: 0,
            hi: width_mask,
            stride: 1,
        }
    }

    pub fn dense(lo: u128, hi: u128) -> Self {
        Self { lo, hi, stride: 1 }
    }

    /// A stride is a fact beyond the span, so a strided full-span interval is
    /// not top.
    pub fn is_top(&self, width_mask: u128) -> bool {
        self.lo == 0 && self.hi >= width_mask && self.stride <= 1
    }

    /// Number of values in the set.  `0` if `hi < lo`.  Saturates: a full-width
    /// top spans `u128::MAX + 1` values.
    pub fn count(&self) -> u128 {
        if self.hi < self.lo {
            0
        } else {
            ((self.hi - self.lo) / self.stride.max(1)).saturating_add(1)
        }
    }

    /// `None` when top.
    #[cfg(test)]
    pub fn upper_exclusive(&self, width_mask: u128) -> Option<u64> {
        if self.hi >= width_mask {
            None
        } else {
            u64::try_from(self.hi + 1).ok()
        }
    }

    /// Meet of two arithmetic progressions. The result stride is `lcm(s1, s2)`
    /// (1 where that overflows or the CRT walk is capped) and `lo` is the first
    /// element `>= max(lo1, lo2)` congruent to both
    /// sources; incompatible residues (or an empty range) yield an empty
    /// interval (`count() == 0`). Sound: the result always contains the real
    /// value set, which is a subset of both operands.
    fn intersect(self, other: Self) -> Self {
        const EMPTY: Interval = Interval {
            lo: 1,
            hi: 0,
            stride: 1,
        };
        let lo_bound = self.lo.max(other.lo);
        let hi = self.hi.min(other.hi);
        if lo_bound > hi {
            return EMPTY;
        }
        let s1 = self.stride.max(1);
        let s2 = other.stride.max(1);
        if s1 == 1 && s2 == 1 {
            return Self {
                lo: lo_bound,
                hi,
                stride: 1,
            };
        }
        // stride = lcm(s1, s2); on overflow fall back to the sound dense meet.
        let Some(lcm) = (s1 / gcd(s1, s2)).checked_mul(s2) else {
            return Self {
                lo: lo_bound,
                hi,
                stride: 1,
            };
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
                Some(x) if x <= hi => Self {
                    lo: x,
                    hi,
                    stride: s2,
                },
                _ => EMPTY,
            };
        }
        // Otherwise cap the walk and fall back to the dense meet: surplus values
        // become dead CFG edges, never a dropped real one.
        const MAX_CRT_STEPS: u128 = 64;
        if lcm / s1 > MAX_CRT_STEPS {
            return Self {
                lo: lo_bound,
                hi,
                stride: 1,
            };
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
                    Self {
                        lo: x,
                        hi,
                        stride: lcm,
                    }
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

    fn union(self, other: Self) -> Self {
        Self {
            lo: self.lo.min(other.lo),
            hi: self.hi.max(other.hi),
            stride: 1, // sound over-approximation for a join
        }
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
            (Some(lo), Some(hi), Some(stride)) if c != 0 && hi <= width_mask => Self {
                lo,
                hi,
                stride: stride.max(1),
            },
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
        if self.hi < self.lo {
            return self;
        }
        let stride = if self.stride.trailing_zeros() >= k {
            self.stride >> k
        } else {
            1
        };
        Self {
            lo: self.lo.checked_shr(k).unwrap_or(0),
            hi: self.hi.checked_shr(k).unwrap_or(0),
            stride: stride.max(1),
        }
    }

    /// `{ x / c : x in self }` (`c == 0` treated as 1).  Floor maps the endpoints
    /// directly.  The stride survives as `stride / c` when `c` divides it, else
    /// floor breaks the progression and it widens to 1, never a stride tighter
    /// than the real spacing.
    fn udiv(self, c: u128) -> Self {
        // As in `shr`: floor-mapping both endpoints would resurrect EMPTY.
        if self.hi < self.lo {
            return self;
        }
        let d = c.max(1);
        let stride = if self.stride.is_multiple_of(d) {
            self.stride / d
        } else {
            1
        };
        Self {
            lo: self.lo / d,
            hi: self.hi / d,
            stride: stride.max(1),
        }
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
    doms: &'f Dominators<NodeId>,
    /// Per guarded value, the `(guard_node, interval)` pairs proven AT that
    /// node: an `If`'s guarded successor edge, or a `Region` every one of whose
    /// predecessor edges bounds the value (a merge whose arms all constrain it).
    guards: FxHashMap<ValueId, Vec<(NodeId, Interval)>>,
    /// Flow-insensitive `[0, max_value]` bounds from KnownBits.
    kb_bounds: SecondaryMap<ValueId, Option<Interval>>,
    /// Resolved intervals, with `InProgress` cutting resolution cycles.
    memo: FxHashMap<(ValueId, NodeId), MemoSlot>,
    /// `dominating_guard` is a pure function of `guards` and `doms`, both
    /// fixed here, and every `range_of` frame asks it at least once.  Without
    /// this each ask walks the value's whole guard list running a
    /// dominator-chain test per entry.
    guard_memo: std::cell::RefCell<FxHashMap<(ValueId, NodeId), Option<Interval>>>,
}

impl<'f> RangeMap<'f> {
    /// Range of `value` valid within `region`.
    ///
    /// A multi-input phi unions each arm's range in that arm's predecessor
    /// region, falling back to a guard on the phi's own output if any arm is
    /// top.  Any other value intersects its dominating guards with the
    /// KnownBits base.  Unconstrained means top.
    pub fn range_of(&mut self, value: ValueId, region: NodeId) -> Interval {
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
        self.memo.insert(key, MemoSlot::InProgress);

        let producer = self.function.producer(value);

        // A monotone constant scaling (`idx << k`, the value a table index
        // feeds into its address) propagates only under a dominating GUARD on
        // the operand: KnownBits scales the stride but loses the upper bound
        // across the scale, so an unguarded `byte * 4` would resolve as a
        // 256-case index.
        let result = if matches!(self.function.node_kind(producer), NodeKind::Phi) {
            self.resolve_phi(producer, region)
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
            // A divide by zero traps, and `eval_int_binary` refuses to fold it,
            // so the node reaches here; `udiv` would read it as a divide by one.
            IntBinaryOp::Div => match rc? {
                0 => return None,
                c => (lhs, ScaleOp::Udiv(c)),
            },
            // Mul is commutative, so the variable may sit on either side.
            IntBinaryOp::Mul => match (self.function.int_const_u128(lhs), rc) {
                (None, Some(c)) => (lhs, ScaleOp::Mul(c)),
                (Some(c), None) => (rhs, ScaleOp::Mul(c)),
                _ => return None,
            },
            _ => return None,
        })
    }

    /// Intersection of every guard on `value` whose `guard_node` dominates
    /// `region`, or `None` when none apply.
    pub(crate) fn dominating_guard(&self, value: ValueId, region: NodeId) -> Option<Interval> {
        if let Some(hit) = self.guard_memo.borrow().get(&(value, region)) {
            return *hit;
        }
        #[cfg(test)]
        GUARD_SCANS.with(|c| c.set(c.get() + 1));
        let verdict = self.guards.get(&value).and_then(|guard_list| {
            guard_list
                .iter()
                .filter(|(guard_region, _)| dominates(self.doms, *guard_region, region))
                .map(|(_, interval)| *interval)
                .reduce(|acc, iv| acc.intersect(iv))
        });
        self.guard_memo
            .borrow_mut()
            .insert((value, region), verdict);
        verdict
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
                NodeKind::If | NodeKind::Call | NodeKind::CallOther { .. } => {
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
    /// [`RangeMap::dominating_guard`] calls, each a linear scan with a
    /// dominance test per entry.
    pub(crate) static GUARD_SCANS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// `u128::MAX` (unconstrained top) when the value carries no typed edge, for a
/// non-integer type, and for a width past the `u128` carrier, where
/// `bit_mask_u128` saturates and the mask stops being a bound the value is
/// inside.  Every consumer that needs a real bound gates on
/// [`crate::opt::known_bits::type_mask_u128`], which answers `None` in all three
/// cases.
fn type_mask_or_top(ty: Option<ValueType>) -> u128 {
    ty.and_then(crate::opt::known_bits::type_mask_u128)
        .unwrap_or(u128::MAX)
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
    let [a, b] = function.producer_inputs_exact::<2>(value).ok()?;
    let (operand, c) = match (function.int_const_u128(a), function.int_const_u128(b)) {
        (None, Some(c)) => (a, c),
        (Some(c), None) => (b, c),
        _ => return None,
    };
    // `bit_mask_u128` saturates to `u128::MAX` past 128 bits, so the shift
    // would wrap mod 2^128 instead of the type's modulus and the `lo <= hi`
    // test would accept a TIGHTER-than-real interval -- the one thing this
    // module must never return.
    let mask = crate::opt::known_bits::type_mask_u128(function.value_type_opt(operand)?)?;
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
    let Some(type_mask) = crate::opt::known_bits::type_mask_u128(ty) else {
        return false;
    };
    let sign_bit = (type_mask >> 1) + 1;
    known[value].zeros & sign_bit != 0
}

/// Builds the flow-insensitive KnownBits bases, extracts per-`If` guard facts,
/// then recovers guards at a merge every predecessor of which bounds the same
/// value.
pub fn compute_value_ranges<'f>(
    function: &'f strider_ir::Function,
    doms: &'f Dominators<NodeId>,
    known: &KnownBitsMap,
) -> RangeMap<'f> {
    let mut kb_bounds: SecondaryMap<ValueId, Option<Interval>> = SecondaryMap::new();
    for node in function.walk() {
        for &value_id in function.node_outputs(node) {
            let kb: KnownBitsFacts = known[value_id];
            if kb.ones == 0 && kb.zeros == 0 {
                continue;
            }
            let Some(ty) = function.value_type_opt(value_id) else {
                continue;
            };
            let Some(type_mask) = crate::opt::known_bits::type_mask_u128(ty) else {
                continue;
            };
            let max_val = kb.max_value(type_mask);
            // `k` trailing known-zero bits means a multiple of `2^k`, capped to
            // the range so the stride stays a valid divisor.
            let tz = kb.zeros.trailing_ones().min(127);
            let stride = (1u128 << tz).min(max_val.max(1));
            if max_val < type_mask || stride > 1 {
                kb_bounds[value_id] = Some(Interval {
                    lo: 0,
                    hi: max_val,
                    stride,
                });
            }
        }
    }

    let mut guards: FxHashMap<ValueId, Vec<(NodeId, Interval)>> = FxHashMap::default();

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

            guards
                .entry(guarded_value)
                .or_default()
                .push((consumer, guard_interval));

            // Back-propagate through `Add(X, const)`: a guard on `idx - K`
            // gives `idx` the shifted bound.
            // Bounded like every other walk here: an `Add` spine is linear in
            // the function, and each step pushes a guard entry that every
            // later `dominating_guard` has to filter.
            const MAX_ADD_BACKPROP: usize = 64;
            let mut cur = guarded_value;
            let mut iv = guard_interval;
            let mut steps = 0usize;
            while let Some((operand, shifted)) = add_operand_shifted_interval(function, cur, iv) {
                if steps == MAX_ADD_BACKPROP {
                    break;
                }
                steps += 1;
                guards.entry(operand).or_default().push((consumer, shifted));
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
        guards.entry(v0).or_default().push((region, union));
    }

    RangeMap {
        function,
        doms,
        guards,
        kb_bounds,
        memo: FxHashMap::default(),
        guard_memo: std::cell::RefCell::new(FxHashMap::default()),
    }
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
    let type_mask = ty.bit_mask_u128();
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
