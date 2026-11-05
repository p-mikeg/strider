//! Dominator-scoped integer ranges per `(value, region)`.
//!
//! SOUNDNESS: `range_of` may widen toward top, but never returns an interval
//! tighter than the runtime value set.

use cranelift_entity::SecondaryMap;
use rustc_hash::{FxHashMap, FxHashSet};

use strider_ir::DominatorTree;
use strider_ir::eval;
use strider_ir::node::{ExtendOp, NodeId, NodeKind, ValueId, ValueType};
use strider_ir::{IRViewer, IRWalker, IntBinaryOp, IntCmpOp};

use crate::opt::known_bits::{KnownBitsFacts, KnownBitsMap};

mod bit_field;
mod interval;
mod trust;

#[cfg(test)]
mod tests;

pub(crate) use interval::{Interval, ScaleOp};
#[cfg(test)]
pub(crate) use trust::{CELL_TEST_SCANS, EQUALITY_SCANS};

use bit_field::{BitField, MaskOperands, bit_field};
pub(crate) use interval::width_mask;

/// Operand hops [`RangeMap::dominating_guard`], [`RangeMap::edge_bound_on`]
/// and [`cast_chain`] take before giving up.
const MAX_GUARD_LADDER: u32 = 8;

/// Deepest chain of `(value, region)` resolutions [`RangeMap::range_of`]
/// follows, so a long phi-of-phi chain cannot overflow the stack. Exhaustion
/// widens to top.
const MAX_RANGE_DEPTH: u32 = 256;

/// Cycle marking for a `(value, region)` query; absent means unvisited.
#[derive(Clone, Copy)]
enum MemoSlot {
    /// Re-entry means a dependency cycle; the back-edge resolves to top.
    InProgress,
    Done(Interval),
}

/// Per-`(value, region)` integer ranges.
pub(crate) struct RangeMap<'f> {
    function: &'f strider_ir::Function,
    doms: DominatorTree<NodeId>,
    /// Per guarded value, the intervals proven at its guard nodes (an `If`
    /// edge's exclusive sink, or a `Region` every predecessor edge of which
    /// bounds the value).
    guard_forests: FxHashMap<ValueId, DomFacts<Interval>>,
    /// Flow-insensitive `[0, max_value]` bounds from KnownBits.
    kb_bounds: SecondaryMap<ValueId, Option<Interval>>,
    /// Resolved intervals, with `InProgress` cutting resolution cycles.
    memo: FxHashMap<(ValueId, NodeId), MemoSlot>,
    /// Per `If`-controlled edge, what taking it proves and about which value,
    /// including edges into a merge, where it holds only for that edge's arm.
    edge_guards: FxHashMap<ValueId, (ValueId, Interval)>,
    /// The edges into a merge whose guards the merge's recovered guard joins.
    merged_edges: FxHashSet<ValueId>,
    /// [`Self::dominating_guard`] answers.
    guard_memo: FxHashMap<(ValueId, NodeId), Option<Interval>>,
    /// What each `And` with a constant mask is the mask of.
    masks: MaskOperands,
    /// Per root, each distinct [`BitField`] of it some guarded value is, with
    /// the guarded values that are exactly that field.
    fields_by_root: FxHashMap<ValueId, Vec<(BitField, Vec<ValueId>)>>,
    /// [`Self::bit_field`] answers, computed on first ask.
    field_memo: SecondaryMap<ValueId, Option<Option<BitField>>>,
    /// Open `range_of` frames, against [`MAX_RANGE_DEPTH`].
    depth: u32,
    /// Whether the query in flight took a depth or cycle cut, whose `top` its
    /// memoized answers depend on; the memo is then dropped at depth 0.
    cut: bool,
    /// Every `If`, by the loads its condition reads.
    cell_tests: trust::CellTests,
    /// KnownBits facts per value.
    known: KnownBitsMap,
    /// Per `(value, k)`, where each `If(y == k)` false edge holds, `y` being
    /// `value` or a zero-extend of it.
    excluded: FxHashMap<(ValueId, u128), DomFacts<()>>,
    /// Per index and range, where [`Self::cell_test_left_unread`] finds an
    /// unread test, as sorted disjoint preorder ranges.
    cell_test_spans: FxHashMap<(ValueId, Interval), Vec<(u32, u32)>>,
}

impl<'f> RangeMap<'f> {
    pub(crate) fn new(function: &'f strider_ir::Function) -> Self {
        build(
            function,
            strider_ir::control_dominator_tree(function),
            crate::opt::known_bits::analyze(function),
        )
    }

    /// Range of `value` valid within `region`; top when unconstrained.
    pub(crate) fn range_of(&mut self, value: ValueId, region: NodeId) -> Interval {
        // Past the `u128` carrier `top` is no bound, and `const_scale` would
        // turn it into a tight one (`top(u128::MAX) >> 127` is at most one).
        let Some(mask) = self
            .function
            .value_type_opt(value)
            .and_then(ValueType::int_mask_u128)
        else {
            return Interval::top(u128::MAX);
        };
        let top = Interval::top(mask);
        let key = (value, region);

        // A cycle's back-edge is top; the frame that opened it still applies
        // its dominating guards.
        match self.memo.get(&key) {
            Some(MemoSlot::Done(iv)) => return *iv,
            Some(MemoSlot::InProgress) => {
                self.cut = true;
                return top;
            }
            None => {}
        }

        // Left out of the memo so a shallower query of the key still resolves.
        if self.depth >= MAX_RANGE_DEPTH {
            self.cut = true;
            return top;
        }
        self.memo.insert(key, MemoSlot::InProgress);
        self.depth += 1;

        let producer = self.function.producer(value);

        // A constant scaling propagates only under a dominating guard on the
        // operand: KnownBits keeps the stride but loses the upper bound, so an
        // unguarded `byte * 4` would resolve as a 256-case index.
        let result = if matches!(self.function.node_kind(producer), NodeKind::Phi) {
            self.resolve_phi(value, mask, region)
        } else if let Some(operand) = self.masks.identity[value] {
            let leaf = self.resolve_leaf(value, region);
            self.range_of(operand, region).intersect(leaf)
        } else {
            let leaf = self.resolve_leaf(value, region);
            match const_scale(self.function, value) {
                Some((operand, scale)) => match self.dominating_guard(operand, region) {
                    Some(guard) => {
                        let inner = self.resolve_leaf_guarded(operand, Some(guard));
                        scale.apply(inner, mask).intersect(leaf)
                    }
                    None => leaf,
                },
                None => leaf,
            }
        };

        self.depth -= 1;
        self.memo.insert(key, MemoSlot::Done(result));
        if self.depth == 0 && std::mem::take(&mut self.cut) {
            self.memo.clear();
        }
        result
    }

    /// The union of the arms' ranges, each queried at the producer of its
    /// edge, met with a guard on the phi's own output at `region`.
    fn resolve_phi(&mut self, value: ValueId, mask: u128, region: NodeId) -> Interval {
        let top = Interval::top(mask);
        let phi_node = self.function.producer(value);
        debug_assert_eq!(self.function.node_outputs(phi_node).first(), Some(&value));
        let Some((_, arms)) = self.phi_arms(phi_node) else {
            return top;
        };

        // `None` once an arm is top: the strides of earlier arms must not
        // survive into the union.
        let mut union: Option<Interval> = None;
        for (edge, arm_val) in arms {
            let arm_range = self.range_of(arm_val, self.function.graph().producer(edge));
            if arm_range.is_top(mask) {
                union = None;
                break;
            }
            union = Some(union.map_or(arm_range, |acc| acc.union(arm_range)));
        }

        let guard = self.dominating_guard(value, region);
        let Some(union) = union else {
            return guard.unwrap_or(top);
        };
        // Arms that fit a guarded truncation make the phi that truncation.
        let narrowed = self.bit_field(value).filter(|f| f.exact).and_then(|whole| {
            let fits = whole.below(union.hi).filter(|&f| f != whole)?;
            self.field_guards(fits, value, region, None)
        });
        [guard, narrowed]
            .into_iter()
            .flatten()
            .fold(union, Interval::intersect)
    }

    /// Intersection of every guard dominating `region` on `value` or on a
    /// value it is built from by [`Self::carrying_operand`] hops (a compare on
    /// `al` bounds the `movzbl %al` an address scales); `None` when none apply.
    fn dominating_guard(&mut self, value: ValueId, region: NodeId) -> Option<Interval> {
        if let Some(hit) = self.guard_memo.get(&(value, region)) {
            return *hit;
        }
        let mut verdict = self.bound_at(value, region);
        let mut hops: Vec<(Step, u128)> = Vec::new();
        let mut cur = value;
        for _ in 0..MAX_GUARD_LADDER {
            let Some((operand, step, mask)) = self.carrying_operand(cur, false) else {
                break;
            };
            hops.push((step, mask));
            if let Some(lifted) = self.bound_at(operand, region).and_then(|below| {
                hops.iter()
                    .rev()
                    .try_fold(below, |iv, &(step, mask)| step.lift(iv, mask))
            }) {
                verdict = Some(verdict.map_or(lifted, |v| v.intersect(lifted)));
            }
            cur = operand;
        }
        self.guard_memo.insert((value, region), verdict);
        verdict
    }

    /// The guards at `region` on `value` and on every value whose bits
    /// enclose its own, met with its phi-arm bound. A guard on wider bits
    /// bounds the field inside: `w < 20` puts `(w >> 4) & 7` at most 1.
    fn bound_at(&mut self, value: ValueId, region: NodeId) -> Option<Interval> {
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
        self.field_guards(field, value, region, own)
    }

    /// `met` met with the guards at `region` on every value other than
    /// `value` whose field encloses `field`, each projected onto `field`.
    fn field_guards(
        &self,
        field: BitField,
        value: ValueId,
        region: NodeId,
        mut met: Option<Interval>,
    ) -> Option<Interval> {
        for (of, over) in self.root_fields(field.root) {
            if !of.encloses(field) {
                continue;
            }
            for &guarded in over {
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

    /// The distinct fields of `root` some guarded value is.
    fn root_fields(&self, root: ValueId) -> &[(BitField, Vec<ValueId>)] {
        self.fields_by_root.get(&root).map_or(&[], Vec::as_slice)
    }

    /// `value` as bits of the value it is extracted from; see [`BitField`].
    fn bit_field(&mut self, value: ValueId) -> Option<BitField> {
        if let Some(hit) = self.field_memo[value] {
            return hit;
        }
        let field = bit_field(self.function, &self.masks, value);
        self.field_memo[value] = Some(field);
        field
    }

    /// The union over a phi's arms of a constant arm's value or the interval
    /// its `If` edge proves of the arm; `None` unless every arm is one of the
    /// two. Covers a loop back edge whose guard sits below the header.
    fn phi_arm_bound(&self, value: ValueId, region: NodeId) -> Option<Interval> {
        let f = self.function;
        let phi_node = f.producer(value);
        if !matches!(f.node_kind(phi_node), NodeKind::Phi) {
            return None;
        }
        let (joining, arms) = self.phi_arms(phi_node)?;
        if !self.doms.dominates(joining, region) {
            return None;
        }
        arms.into_iter()
            .map(|(edge, arm)| match f.int_const_u128(arm) {
                Some(k) => Some(Interval::dense(k, k)),
                None => self.edge_bound_on(edge, arm),
            })
            .collect::<Option<Vec<_>>>()?
            .into_iter()
            .reduce(Interval::union)
    }

    /// What taking `edge` proves about `arm`, lifted up the hops from the
    /// compared value (or a value it zero-extends) to the arm.
    fn edge_bound_on(&self, edge: ValueId, arm: ValueId) -> Option<Interval> {
        let (guarded, iv) = *self.edge_guards.get(&edge)?;
        let f = self.function;
        let same: Vec<ValueId> = cast_chain(f, guarded, |k| {
            matches!(k, NodeKind::Extend(ExtendOp::ZeroExtend))
        })
        .collect();
        let mut hops: Vec<(Step, u128)> = Vec::new();
        let mut cur = arm;
        while !same.contains(&cur) {
            if hops.len() == MAX_GUARD_LADDER as usize {
                return None;
            }
            let (operand, step, mask) = self.carrying_operand(cur, true)?;
            hops.push((step, mask));
            cur = operand;
        }
        hops.iter()
            .rev()
            .try_fold(iv, |acc, &(step, mask)| step.lift(acc, mask))
    }

    /// `x` when `value` is an `And(x, c)` whose mask clears only bits known zero
    /// in `x`, i.e. the same value as `x`.
    pub(crate) fn identity_mask_operand(&self, value: ValueId) -> Option<ValueId> {
        self.masks.identity[value]
    }

    /// The guards recorded on `value` itself whose node dominates `region`,
    /// met.
    fn guard_at(&self, value: ValueId, region: NodeId) -> Option<Interval> {
        #[cfg(test)]
        GUARD_SCANS.with(|c| c.set(c.get() + 1));
        self.guard_forests
            .get(&value)?
            .meet_dominating(self.doms.span(region)?)
    }

    /// The operand whose bound `value` inherits through a cast, a mask, or
    /// with `scales` a constant scaling, with the step and `value`'s width
    /// mask; `None` when no hop is sound.
    fn carrying_operand(&self, value: ValueId, scales: bool) -> Option<(ValueId, Step, u128)> {
        let f = self.function;
        let (x, step) = operand_step(f, value)?;
        let carries = match step {
            Step::ZeroExtend | Step::And(_) => true,
            Step::Truncate => f.value_type_opt(value)?.int_mask_u128().is_some(),
            step => scales && scale_of(step).is_some(),
        };
        carries.then(|| (x, step, width_mask(f, value)))
    }

    /// `phi_node`'s `Region` and its `(edge, arm)` pairs; `None` for fewer
    /// than two arms.
    fn phi_arms(&self, phi_node: NodeId) -> Option<(NodeId, Vec<(ValueId, ValueId)>)> {
        let f = self.function;
        let arms: Vec<(ValueId, ValueId)> = f.phi_arms(phi_node).collect();
        (arms.len() >= 2).then(|| (f.phi_region(phi_node), arms))
    }

    /// Whether `sum`, an `Add` output, may range narrower than its type at
    /// some region.
    pub(crate) fn may_bound_sum(&self, sum: ValueId) -> bool {
        self.kb_bounds[sum].is_some()
            || self.guard_forests.contains_key(&sum)
            || self.fields_by_root.contains_key(&sum)
    }

    /// Whether `x` agrees with every bit KnownBits proves of `value`.
    pub(crate) fn admits_known_bits(&self, value: ValueId, x: u128) -> bool {
        let kb = self.known[value];
        x & kb.ones == kb.ones && x & kb.zeros == 0
    }

    /// `value`'s KnownBits bound, or its type's top.
    pub(crate) fn kb_bound(&self, value: ValueId) -> Interval {
        self.resolve_leaf_guarded(value, None)
    }

    /// Intersects `value`'s dominating guards with its KnownBits base.
    fn resolve_leaf(&mut self, value: ValueId, region: NodeId) -> Interval {
        let guard = self.dominating_guard(value, region);
        self.resolve_leaf_guarded(value, guard)
    }

    /// [`Self::resolve_leaf`] against an already-computed dominating guard.
    fn resolve_leaf_guarded(&self, value: ValueId, guard: Option<Interval>) -> Interval {
        let type_mask = width_mask(self.function, value);
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
    /// [`DomFacts::meet_dominating`] probes.
    pub(crate) static GUARD_PROBES: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// The values, sorted, the phi `value` can hold when each of its arms is a
/// constant or such a phi; `None` for any other arm, or past `cap` values or
/// `cap` phis. Exact where [`RangeMap::range_of`] is a hull.
pub(crate) fn phi_constants(
    function: &strider_ir::Function,
    value: ValueId,
    cap: usize,
) -> Option<Vec<u128>> {
    let first = function.producer(value);
    if !matches!(function.node_kind(first), NodeKind::Phi) {
        return None;
    }
    let mut values: std::collections::BTreeSet<u128> = std::collections::BTreeSet::new();
    let mut seen: rustc_hash::FxHashSet<NodeId> = rustc_hash::FxHashSet::default();
    seen.insert(first);
    let mut stack = vec![first];
    while let Some(phi) = stack.pop() {
        for arm in function.phi_data_inputs(phi) {
            if let Some(k) = function.int_const_u128(arm) {
                values.insert(k);
                if values.len() > cap {
                    return None;
                }
                continue;
            }
            let producer = function.producer(arm);
            if !matches!(function.node_kind(producer), NodeKind::Phi) {
                return None;
            }
            if seen.insert(producer) {
                if seen.len() > cap {
                    return None;
                }
                stack.push(producer);
            }
        }
    }
    Some(values.into_iter().collect())
}

/// For `value = Add(X, c)` guarded by `interval`, `X` and `interval - c` mod
/// its width; `None` for any other value or when the shift wraps.
fn add_operand_shifted_interval(
    function: &strider_ir::Function,
    value: ValueId,
    interval: Interval,
) -> Option<(ValueId, Interval)> {
    let (operand, c) = crate::const_eval::var_and_const_of(
        function,
        function.producer(value),
        NodeKind::IntBinaryOp(IntBinaryOp::Add),
    )?;
    // Past 128 bits the shift would wrap mod 2^128, not the type's, and could
    // return a tighter-than-real interval.
    let mask = function.value_type_opt(operand)?.int_mask_u128()?;
    let lo = interval.lo.wrapping_sub(c) & mask;
    let hi = interval.hi.wrapping_sub(c) & mask;
    (lo <= hi).then_some((operand, Interval::dense(lo, hi)))
}

/// `rotl(x, r)`, `Or(x << r, x >> (w - r))`, held below `2^r` keeps `x`'s
/// low `w - r` bits zero and its top `r` bits the rotated value, so `x` is
/// that value shifted up by `w - r`.
fn rotate_operand_interval(
    function: &strider_ir::Function,
    value: ValueId,
    interval: Interval,
) -> Option<(ValueId, Interval)> {
    if !matches!(
        function.kind_of_value(value),
        NodeKind::IntBinaryOp(IntBinaryOp::Or)
    ) {
        return None;
    }
    let w = u32::try_from(function.value_type_opt(value)?.bit_width())
        .ok()
        .filter(|&w| w <= 128)?;
    let shift_of = |v: ValueId, op: IntBinaryOp| -> Option<(ValueId, u32)> {
        match (operand_step(function, v)?, op) {
            ((x, Step::ShiftLeft(k)), IntBinaryOp::ShiftLeft)
            | ((x, Step::ShiftRight(k)), IntBinaryOp::ShiftRight) => Some((x, k)),
            _ => None,
        }
    };
    let [a, b] = function.producer_inputs_exact::<2>(value).ok()?;
    let ((x, r), (y, down)) = shift_of(a, IntBinaryOp::ShiftLeft)
        .zip(shift_of(b, IntBinaryOp::ShiftRight))
        .or_else(|| {
            shift_of(b, IntBinaryOp::ShiftLeft).zip(shift_of(a, IntBinaryOp::ShiftRight))
        })?;
    if x != y || r == 0 || r >= w || r + down != w || interval.hi >> r != 0 {
        return None;
    }
    let stride = if interval.stride >> r == 0 {
        interval.stride << down
    } else {
        1 << down
    };
    Some((
        x,
        Interval::new(interval.lo << down, interval.hi << down, stride),
    ))
}

/// How a value is built from one variable operand. A shift or divide takes
/// its constant on the right only.
#[derive(Clone, Copy)]
pub(crate) enum Step {
    ZeroExtend,
    SignExtend,
    Truncate,
    ShiftLeft(u32),
    ShiftRight(u32),
    Div(u128),
    Mul(u128),
    And(u128),
}

impl Step {
    /// What `iv`, a bound on the operand, proves of the value, `mask` its
    /// width mask; `None` where nothing carries.
    fn lift(self, iv: Interval, mask: u128) -> Option<Interval> {
        match self {
            Step::ZeroExtend => Some(iv),
            Step::Truncate => (iv.hi <= mask).then_some(iv),
            // `x & c <= x`: only the upper bound survives.
            Step::And(_) => Some(Interval::dense(0, iv.hi)),
            step => Some(scale_of(step)?.apply(iv, mask)),
        }
    }

    /// The exact image of `iv` under a step [`strip_to_cell`] takes, at the
    /// value's `mask`; `None` where no interval is exactly that image.
    pub(crate) fn image(self, iv: Interval, mask: u128) -> Option<Interval> {
        match self {
            Step::ZeroExtend | Step::And(_) => Some(iv),
            Step::Truncate if iv.hi <= mask => Some(iv),
            Step::Truncate => {
                (iv.stride == 1 && iv.hi - iv.lo >= mask).then(|| Interval::top(mask))
            }
            step => match scale_of(step)? {
                ScaleOp::Mul(c) => {
                    let hi = iv.hi.checked_mul(c).filter(|&hi| hi <= mask)?;
                    Some(Interval::new(iv.lo * c, hi, iv.stride.checked_mul(c)?))
                }
                ScaleOp::Udiv(c) if iv.stride.is_multiple_of(c) => {
                    Some(Interval::new(iv.lo / c, iv.hi / c, iv.stride / c))
                }
                ScaleOp::Udiv(c) => (iv.stride <= c).then(|| Interval::dense(iv.lo / c, iv.hi / c)),
            },
        }
    }
}

/// The variable operand `value`'s producer reads and how; `None` for any
/// other producer.
fn operand_step(f: &strider_ir::Function, value: ValueId) -> Option<(ValueId, Step)> {
    let rhs_const = || -> Option<(ValueId, u128)> {
        let [x, k] = f.producer_inputs_exact::<2>(value).ok()?;
        Some((x, f.int_const_u128(k)?))
    };
    let shift = |step: fn(u32) -> Step| {
        rhs_const().and_then(|(x, k)| Some((x, step(u32::try_from(k).ok()?))))
    };
    let var_const = || crate::const_eval::var_and_const(f, f.producer(value));
    let operand = || f.int_inputs(value).next();
    match *f.kind_of_value(value) {
        NodeKind::Extend(ExtendOp::ZeroExtend) => Some((operand()?, Step::ZeroExtend)),
        NodeKind::Extend(ExtendOp::SignExtend) => Some((operand()?, Step::SignExtend)),
        NodeKind::Truncate => Some((operand()?, Step::Truncate)),
        NodeKind::IntBinaryOp(IntBinaryOp::ShiftLeft) => shift(Step::ShiftLeft),
        NodeKind::IntBinaryOp(IntBinaryOp::ShiftRight) => shift(Step::ShiftRight),
        NodeKind::IntBinaryOp(IntBinaryOp::Div) => rhs_const().map(|(x, c)| (x, Step::Div(c))),
        NodeKind::IntBinaryOp(IntBinaryOp::Mul) => var_const().map(|(x, c)| (x, Step::Mul(c))),
        NodeKind::IntBinaryOp(IntBinaryOp::And) => var_const().map(|(x, c)| (x, Step::And(c))),
        _ => None,
    }
}

/// The operand and scaling when `value` is a monotone constant scaling of one
/// variable operand.
fn const_scale(f: &strider_ir::Function, value: ValueId) -> Option<(ValueId, ScaleOp)> {
    let (x, step) = operand_step(f, value)?;
    Some((x, scale_of(step)?))
}

/// The monotone constant scaling `step` is, if any.
pub(crate) fn scale_of(step: Step) -> Option<ScaleOp> {
    match step {
        Step::ShiftLeft(k) => ScaleOp::shl(k),
        Step::ShiftRight(k) => ScaleOp::shr(k),
        // A divide by zero traps and is not folded; `udiv` would read it as /1.
        Step::Div(0) => None,
        Step::Div(c) => Some(ScaleOp::Udiv(c)),
        Step::Mul(c) => Some(ScaleOp::Mul(c)),
        _ => None,
    }
}

/// Steps any one walk over a value chain or cone takes here; a walk reaching
/// it answers the conservative way.
pub(crate) const MAX_WALK: usize = 64;

/// A value read as the cell [`strip_to_cell`] reduces it to.
pub(crate) struct Stripped {
    pub(crate) cell: ValueId,
    /// Each value stripped, outermost first, with the step building it.
    pub(crate) steps: Vec<(ValueId, Step)>,
}

/// What `v` reduces to through zero-extends, truncates, the constant `And`s
/// `through_mask` accepts and nonzero constant scalings; `None` past
/// [`MAX_WALK`] steps.
pub(crate) fn strip_to_cell(
    f: &strider_ir::Function,
    v: ValueId,
    through_mask: impl Fn(ValueId) -> bool,
) -> Option<Stripped> {
    let mut base = v;
    let mut chain = Vec::new();
    for _ in 0..MAX_WALK {
        let Some((next, step)) = operand_step(f, base) else {
            return Some(Stripped {
                cell: base,
                steps: chain,
            });
        };
        let strips = match step {
            Step::ZeroExtend => true,
            Step::Truncate => f.value_type_opt(base).is_some(),
            Step::And(_) => through_mask(base),
            Step::Mul(0) => false,
            step => scale_of(step).is_some(),
        };
        if !strips {
            return Some(Stripped {
                cell: base,
                steps: chain,
            });
        }
        chain.push((base, step));
        base = next;
    }
    None
}

/// Facts at nodes in dominator-tree preorder, each pre-met with its
/// dominators' facts, so the meet holding at a node is one search.
struct DomFacts<T> {
    entries: Vec<DomEntry<T>>,
    /// `max_post[l][i]`: the largest `post` among `entries[i..i + 2^l]`.
    max_post: Vec<Vec<u32>>,
}

struct DomEntry<T> {
    pre: u32,
    post: u32,
    met: T,
}

impl<T: Copy> DomFacts<T> {
    /// Drops `facts`' nodes outside the tree.
    fn new(
        doms: &DominatorTree<NodeId>,
        facts: impl IntoIterator<Item = (NodeId, T)>,
        meet: impl Fn(T, T) -> T,
    ) -> Self {
        let mut entries: Vec<DomEntry<T>> = facts
            .into_iter()
            .filter_map(|(node, met)| {
                let (pre, post) = doms.span(node)?;
                Some(DomEntry { pre, post, met })
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
            if let Some(&parent) = open.last() {
                entries[i].met = meet(entries[i].met, entries[parent].met);
            }
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

    /// The facts whose nodes enclose `span`, met.
    fn meet_dominating(&self, (pre, post): (u32, u32)) -> Option<T> {
        // An entry up to `end` encloses the query iff its `post` is not below
        // `post`; the last such is the nearest dominating fact.
        let mut end = self.entries.partition_point(|e| e.pre <= pre);
        for level in (0..self.max_post.len()).rev() {
            let width = 1usize << level;
            if end >= width && self.max_post[level][end - width] < post {
                end -= width;
            }
        }
        #[cfg(test)]
        GUARD_PROBES.with(|c| c.set(c.get() + 1));
        Some(self.entries[end.checked_sub(1)?].met)
    }
}

/// Records `interval` as proven at `node`, met with any bound already there.
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
    known: KnownBitsMap,
) -> RangeMap<'f> {
    let mut kb_bounds: SecondaryMap<ValueId, Option<Interval>> = SecondaryMap::new();
    let mut masks = MaskOperands::default();
    for node in function.walk() {
        masks.record(function, node, &known);
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
            let stride = eval::bit(tz).min(max_val.max(1));
            if max_val < type_mask || stride > 1 {
                kb_bounds[value_id] = Some(Interval::new(0, max_val, stride));
            }
        }
    }

    let mut guards: FxHashMap<ValueId, FxHashMap<NodeId, Interval>> = FxHashMap::default();
    let mut edge_guards: FxHashMap<ValueId, (ValueId, Interval)> = FxHashMap::default();
    let mut merged_edges: FxHashSet<ValueId> = FxHashSet::default();

    let ifs: Vec<NodeId> = function.walk_kind(|k| matches!(k, NodeKind::If)).collect();
    for &if_node in &ifs {
        let if_outputs = function.node_outputs(if_node);
        if if_outputs.len() < 2 {
            continue;
        }
        let true_ctrl = if_outputs[NodeKind::IF_TRUE_OUT_SLOT];
        let false_ctrl = if_outputs[NodeKind::IF_FALSE_OUT_SLOT];

        let cond_value = function.if_cond(if_node);

        for (edge_ctrl, edge_taken) in [(true_ctrl, true), (false_ctrl, false)] {
            let Some((guarded_value, guard_interval)) =
                guard_from_compare(function, cond_value, edge_taken, &known)
            else {
                continue;
            };

            edge_guards.insert(edge_ctrl, (guarded_value, guard_interval));

            let Some(consumer) = guard_sink(function, edge_ctrl) else {
                continue;
            };

            add_guard(&mut guards, guarded_value, consumer, guard_interval);

            // Back-propagate through `Add(X, const)`, where a guard on `idx - K`
            // gives `idx` the shifted bound, and through a rotate.
            let mut cur = guarded_value;
            let mut iv = guard_interval;
            for _ in 0..MAX_WALK {
                let Some((operand, shifted)) = add_operand_shifted_interval(function, cur, iv)
                    .or_else(|| rotate_operand_interval(function, cur, iv))
                else {
                    break;
                };
                add_guard(&mut guards, operand, consumer, shifted);
                cur = operand;
                iv = shifted;
            }
        }
    }

    // A value bounded on every predecessor edge of a merge is bounded there
    // by the union. One level: a non-`If` predecessor blocks the join.
    for region in function.walk_kind(|k| matches!(k, NodeKind::Region)) {
        let pred_edges: Vec<ValueId> = function.node_inputs(region).into_iter().collect();
        if pred_edges.len() < 2 {
            continue;
        }
        let Some(per_edge) = pred_edges
            .iter()
            .map(|e| edge_guards.get(e).copied())
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
        merged_edges.extend(pred_edges);
    }

    let mut excluded: FxHashMap<(ValueId, u128), Vec<(NodeId, ())>> = FxHashMap::default();
    for &if_node in &ifs {
        let Some(&false_ctrl) = function
            .node_outputs(if_node)
            .get(NodeKind::IF_FALSE_OUT_SLOT)
        else {
            continue;
        };
        let Some(sink) = guard_sink(function, false_ctrl) else {
            continue;
        };
        let Some((x, k)) = crate::const_eval::var_and_const_of(
            function,
            function.producer(function.if_cond(if_node)),
            NodeKind::IntCmpOp(IntCmpOp::Equal),
        ) else {
            continue;
        };
        // A zero-extend is the same value, so its equality excludes `k`
        // from every value it extends.
        let mut y = x;
        loop {
            if k <= width_mask(function, y) {
                excluded.entry((y, k)).or_default().push((sink, ()));
            }
            match operand_step(function, y) {
                Some((below, Step::ZeroExtend)) => y = below,
                _ => break,
            }
        }
    }
    let excluded = excluded
        .into_iter()
        .map(|(key, sinks)| (key, DomFacts::new(&doms, sinks, |(), ()| ())))
        .collect();
    let guard_forests = guards
        .iter()
        .map(|(&value, by_node)| {
            // Meets run out of nesting order, which a power-of-two stride
            // makes associative.
            debug_assert!(by_node.values().all(|iv| iv.stride.is_power_of_two()));
            let facts = by_node.iter().map(|(&node, &iv)| (node, iv));
            (value, DomFacts::new(&doms, facts, Interval::intersect))
        })
        .collect();
    let mut by_field: FxHashMap<BitField, Vec<ValueId>> = FxHashMap::default();
    for &value in guards.keys() {
        if let Some(field) = bit_field(function, &masks, value) {
            by_field.entry(field).or_default().push(value);
        }
    }
    let mut fields_by_root: FxHashMap<ValueId, Vec<(BitField, Vec<ValueId>)>> =
        FxHashMap::default();
    for (field, over) in by_field {
        fields_by_root
            .entry(field.root)
            .or_default()
            .push((field, over));
    }
    RangeMap {
        function,
        doms,
        guard_forests,
        edge_guards,
        merged_edges,
        kb_bounds,
        memo: FxHashMap::default(),
        guard_memo: FxHashMap::default(),
        masks,
        fields_by_root,
        field_memo: SecondaryMap::new(),
        depth: 0,
        cut: false,
        cell_tests: trust::CellTests::new(function, &ifs),
        known,
        excluded,
        cell_test_spans: FxHashMap::default(),
    }
}

/// The upper bound a `Less`/`Sless` condition puts on its variable on the
/// `edge_taken` edge; `None` for a lower bound only. `Sless` needs KnownBits
/// sign bit 0.
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
    let (guarded, n) = crate::const_eval::var_and_const(function, function.producer(cmp_value))?;
    // Const on RHS means `v < N`; const on LHS means `N < v`.
    let const_on_rhs = function.producer_inputs_exact::<2>(cmp_value).ok()?[0] == guarded;

    let ty = function.value_type_opt(guarded)?;
    debug_assert!(ty.is_integer());
    let type_mask = ty.int_mask_u128()?;
    if ty == ValueType::I1 {
        return None;
    }
    let sign_bit = (type_mask >> 1) + 1;
    if op == IntCmpOp::Sless && known[guarded].zeros & sign_bit == 0 {
        return None;
    }
    // A negative `Sless` constant decides the compare and bounds nothing.
    if op == IntCmpOp::Sless && n & !(type_mask >> 1) != 0 {
        return None;
    }

    match (const_on_rhs, edge_taken) {
        (true, true) => {
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

/// `value` and the values below it through the casts `through` accepts, at
/// most [`MAX_GUARD_LADDER`] of them.
fn cast_chain<'a>(
    f: &'a strider_ir::Function,
    value: ValueId,
    through: impl Fn(&NodeKind) -> bool + 'a,
) -> impl Iterator<Item = ValueId> + 'a {
    std::iter::successors(Some(value), move |&v| {
        through(f.kind_of_value(v))
            .then(|| f.int_inputs(v).next())
            .flatten()
    })
    .take(MAX_GUARD_LADDER as usize)
}

/// The sink of `edge` when reached only via that edge, so its fact holds
/// there; `None` for a `Region` (a merge).
fn guard_sink(function: &strider_ir::Function, edge: ValueId) -> Option<NodeId> {
    single_control_consumer(function, edge)
        .filter(|&sink| !matches!(function.node_kind(sink), NodeKind::Region))
}

/// The one sink of a control edge; `None` for a dead edge or for two sinks,
/// either of which may not have passed the compare.
fn single_control_consumer(function: &strider_ir::Function, ctrl_val: ValueId) -> Option<NodeId> {
    let mut uses = function
        .graph()
        .value_uses(ctrl_val)
        .map(|(consumer, _slot)| consumer);
    let only = uses.next()?;
    uses.next().is_none().then_some(only)
}
