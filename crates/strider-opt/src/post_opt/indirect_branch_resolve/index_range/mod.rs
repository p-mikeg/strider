//! Dominator-scoped per-`(value, region)` integer range analysis, read by
//! the jump-table classifier above it and nothing else.
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
    /// Per guarded value, the intervals proven AT its guard nodes (an `If`'s
    /// guarded successor edge, or a `Region` every one of whose predecessor
    /// edges bounds the value) on the dominator tree, so a lookup climbs only
    /// the guards dominating the query point.
    guard_forests: FxHashMap<ValueId, DomFacts<Interval>>,
    /// Flow-insensitive `[0, max_value]` bounds from KnownBits.
    kb_bounds: SecondaryMap<ValueId, Option<Interval>>,
    /// Resolved intervals, with `InProgress` cutting resolution cycles.
    memo: FxHashMap<(ValueId, NodeId), MemoSlot>,
    /// Per `If`-controlled edge, what taking it proves and about which value.
    ///
    /// Distinct from `guard_forests`, keyed by the node a guard holds AT, which
    /// so cannot record one whose edge lands on a merge. An edge fact holds
    /// only for the value ARRIVING on that edge, which is what a phi arm is.
    edge_guards: FxHashMap<ValueId, (ValueId, Interval)>,
    /// The edges into a merge whose guards the merge's recovered guard joins.
    merged_edges: FxHashSet<ValueId>,
    /// `dominating_guard` is a pure function of the guards and `doms`, both
    /// fixed here, and every `range_of` frame asks it at least once.
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
    /// Whether the query in flight took a depth or cycle cut. Its frames fold
    /// the cut's `top` into answers the key does not record the cut in, so
    /// the memo is dropped rather than handed to the next query.
    cut: bool,
    /// Every `If`, by the loads its condition reads.
    cell_tests: trust::CellTests,
    /// The KnownBits facts the bounds above are built from.
    known: KnownBitsMap,
    /// Per `(value, k)`, where each `If(y == k)` false edge holds, `y` being
    /// `value` or a zero-extend of it.
    excluded: FxHashMap<(ValueId, u128), DomFacts<()>>,
    /// Per index and range, where [`Self::cell_test_left_unread`] finds an
    /// unread test, as sorted disjoint preorder ranges. The index's strip is
    /// a function of the index.
    cell_test_spans: FxHashMap<(ValueId, Interval), Vec<(u32, u32)>>,
}

impl<'f> RangeMap<'f> {
    /// The ranges of `function`: builds the flow-insensitive KnownBits bases,
    /// extracts per-`If` guard facts, then recovers guards at a merge every
    /// predecessor of which bounds the same value.
    pub(crate) fn new(function: &'f strider_ir::Function) -> Self {
        build(
            function,
            strider_ir::control_dominator_tree(function),
            crate::opt::known_bits::analyze(function),
        )
    }

    /// The function these ranges are of.
    #[must_use]
    pub(crate) fn function(&self) -> &'f strider_ir::Function {
        self.function
    }

    /// Range of `value` valid within `region`.
    ///
    /// A multi-input phi unions each arm's range in that arm's predecessor
    /// region, falling back to a guard on the phi's own output if any arm is
    /// top.  Any other value intersects its dominating guards with the
    /// KnownBits base.  Unconstrained means top.
    pub(crate) fn range_of(&mut self, value: ValueId, region: NodeId) -> Interval {
        // A width past the `u128` carrier has no mask the value is inside, so
        // `Interval::top` there is not a bound. Refusing locally keeps the
        // non-bound out of `const_scale`, which would otherwise turn it into a
        // tight claim (`top(u128::MAX) >> 127` is at most one).
        let Some(mask) = self
            .function
            .value_type_opt(value)
            .and_then(ValueType::int_mask_u128)
        else {
            return Interval::top(u128::MAX);
        };
        // Past the gate the mask exists, so the two bail-outs below share it.
        let top = Interval::top(mask);
        let key = (value, region);

        // A cycle, cut by returning top for the back-edge; the frame that
        // opened it still applies its dominating guards.
        match self.memo.get(&key) {
            Some(MemoSlot::Done(iv)) => return *iv,
            Some(MemoSlot::InProgress) => {
                self.cut = true;
                return top;
            }
            None => {}
        }

        // Budget exhausted: top, left out of the memo so a shallower query of
        // the same key still resolves it.
        if self.depth >= MAX_RANGE_DEPTH {
            self.cut = true;
            return top;
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

    /// `region` applies only to a guard on the phi's own output; each arm is
    /// queried at the producer of the edge it arrives on.
    fn resolve_phi(&mut self, value: ValueId, mask: u128, region: NodeId) -> Interval {
        let top = Interval::top(mask);
        let phi_node = self.function.producer(value);
        debug_assert_eq!(self.function.node_outputs(phi_node).first(), Some(&value));
        // A guard dominating an arm's edge holds on that edge; one holding on
        // another incoming path must not apply.
        let Some((_, arms)) = self.phi_arms(phi_node) else {
            return top;
        };

        // `None` once an arm is top: the union is top too, and the arms taken
        // so far must not survive it, their stride being one only they obey.
        let mut union: Option<Interval> = None;
        for (edge, arm_val) in arms {
            let arm_range = self.range_of(arm_val, self.function.graph().producer(edge));
            if arm_range.is_top(mask) {
                union = None;
                break;
            }
            union = Some(union.map_or(arm_range, |acc| acc.union(arm_range)));
        }

        // A guard holding at the query point is valid for every arm, and is
        // all that bounds the phi once one of them is top.
        match (union, self.dominating_guard(value, region)) {
            (Some(union), Some(guard)) => union.intersect(guard),
            (Some(union), None) => union,
            (None, guard) => guard.unwrap_or(top),
        }
    }

    /// Intersection of every guard on `value` whose `guard_node` dominates
    /// `region`, or `None` when none apply.
    ///
    /// Also carries up a guard recorded on a value `value` is built from by a
    /// chain of [`Self::carrying_operand`] hops: a compare on `al` bounds
    /// the `movzbl %al` an addressing mode scales, and without the walk the
    /// index falls back to the KnownBits width bound and reads the whole table
    /// domain.
    fn dominating_guard(&mut self, value: ValueId, region: NodeId) -> Option<Interval> {
        if let Some(hit) = self.guard_memo.get(&(value, region)) {
            return *hit;
        }
        let mut verdict = self.bound_at(value, region);
        // The hops from `value` down to `cur`, each lifting its operand's
        // bound to the value above it.
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

    /// What holds on `value` itself at `region`: the guards recorded on it and
    /// on every other value whose bits enclose its own, met with what its
    /// incoming edges prove if it is a phi.
    ///
    /// MIPS64 guards `w >> 26` with `srl` and indexes by an `ext` of the same
    /// six bits, `(sext(w) << 32) >> 58`: two nodes, one value.  A guard on
    /// wider bits bounds the field inside them: `w < 20` puts `(w >> 4) & 7`
    /// at most 1.
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
        let mut met = own;
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

    /// What taking `edge` proves about the value `arm` arriving on it, mapped up
    /// the hops between the compared value and the arm: the compare is on the
    /// register, the arm on a widened, masked or already-scaled copy of it.
    ///
    /// The hops are collected downward and replayed upward, so each one sees
    /// the interval its own operand really carries.
    ///
    /// A zero-extend is the identity on the value, so the compared value
    /// stands for each value it zero-extends: a guard on `zext32(b)` bounds
    /// an arm `zext64(b)`.
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
    /// met from `region` up towards the entry.
    fn guard_at(&self, value: ValueId, region: NodeId) -> Option<Interval> {
        #[cfg(test)]
        GUARD_SCANS.with(|c| c.set(c.get() + 1));
        self.guard_forests
            .get(&value)?
            .meet_dominating(self.doms.span(region)?)
    }

    /// The operand whose bound `value` inherits through a cast or a mask,
    /// with the step and `value`'s width mask; with `scales`, through a
    /// monotone constant scaling too, which a bound survives as a mapped
    /// interval.  `None` when no hop is sound, which ends the walk.
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

    /// Per arm of `phi_node`, the control edge it arrives on paired with the
    /// arm value, in the joining `Region`'s input order, and that `Region`.
    /// `None` for fewer than two arms.
    fn phi_arms(&self, phi_node: NodeId) -> Option<(NodeId, Vec<(ValueId, ValueId)>)> {
        let f = self.function;
        let arms: Vec<(ValueId, ValueId)> = f.phi_arms(phi_node).collect();
        (arms.len() >= 2).then(|| (f.phi_region(phi_node), arms))
    }

    /// Whether `sum`, an `Add` output, can range narrower than its type at
    /// some region: only its KnownBits bound, a guard on it, or a guard on a
    /// field of it narrows one.
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
    /// Dominating-guard lookups, each one probe: the cost a query pays, which
    /// must track neither the chain's depth nor the value's guard count.
    pub(crate) static GUARD_PROBES: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// The values the phi `value` can hold when each of its arms is a constant or
/// a phi of the same kind, sorted; `None` for any other arm, or past `cap`
/// values or `cap` phis.
///
/// Exact where [`RangeMap::range_of`] is a hull: the union of `{0, 3, 5}` is
/// `[0, 5]`.  A value reaches a web of such phis only through one of its
/// constant arms, so a cycle among them adds nothing.
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

/// For `value = Add(X, IntConst(c))`, the bound `X` inherits from a guard on
/// `X + c`: `interval` shifted by `-c` mod the operand width.  `None` if the
/// value is not such an add, or the shift wraps (a straddling interval has no
/// single `[lo, hi]` form).
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
    // Past 128 bits there is no `u128` modulus, so the shift would wrap mod
    // 2^128 instead of the type's and the `lo <= hi` test would accept a
    // TIGHTER-than-real interval, the one thing this module must never return.
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

/// How a value is built from one variable operand.  A shift or divide takes
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
            // Zero extension is the identity on the unsigned value.
            Step::ZeroExtend => Some(iv),
            // Truncation is the identity only below the narrower type's mask.
            Step::Truncate => (iv.hi <= mask).then_some(iv),
            // Masking only clears bits, so `x & c <= x` carries the upper
            // bound while the low end and the stride do not survive.
            Step::And(_) => Some(Interval::dense(0, iv.hi)),
            step => Some(scale_of(step)?.apply(iv, mask)),
        }
    }

    /// The image of `iv` under a step [`strip_to_cell`] takes, at the
    /// value's `mask`, or `None` where no interval is exactly that image: a
    /// scaling that wraps, a divide that leaves gaps, a truncate of part of
    /// a span.
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

/// If `value` is a monotone constant scaling of one variable operand
/// (`x << k`, `x * c`, `x >> k`, `x / c`), the operand and how to transform
/// its range.  The constant must be the RHS for the non-commutative shapes.
fn const_scale(f: &strider_ir::Function, value: ValueId) -> Option<(ValueId, ScaleOp)> {
    let (x, step) = operand_step(f, value)?;
    Some((x, scale_of(step)?))
}

/// The monotone constant scaling `step` is, if any.
pub(crate) fn scale_of(step: Step) -> Option<ScaleOp> {
    match step {
        Step::ShiftLeft(k) => ScaleOp::shl(k),
        Step::ShiftRight(k) => ScaleOp::shr(k),
        // A divide by zero traps, and `eval::int_binary` refuses to fold it,
        // so the node reaches here; `udiv` would read it as a divide by one.
        Step::Div(0) => None,
        Step::Div(c) => Some(ScaleOp::Udiv(c)),
        Step::Mul(c) => Some(ScaleOp::Mul(c)),
        _ => None,
    }
}

/// Steps any one walk over a value chain or cone takes here: a cell strip,
/// a field address, a test's cone, an `Add` spine. A real chain is a few
/// steps; the cap only keeps a pathological one off the cost centre, and
/// every walk that reaches it answers the conservative way.
pub(crate) const MAX_WALK: usize = 64;

/// A value read as the cell [`strip_to_cell`] reduces it to.
pub(crate) struct Stripped {
    pub(crate) cell: ValueId,
    /// Each value stripped, outermost first, with the step building it.
    pub(crate) steps: Vec<(ValueId, Step)>,
}

/// What `v` reduces to through zero-extends, truncates, the `And`s with a
/// constant `through_mask` accepts and nonzero constant scalings. `None` past
/// [`MAX_WALK`] steps, which answers "width-only".
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

/// Facts at nodes in dominator-tree preorder, each met with every fact whose
/// node dominates it, so the meet holding at a node is one search.
struct DomFacts<T> {
    entries: Vec<DomEntry<T>>,
    /// `max_post[l][i]`: the largest `post` among `entries[i..i + 2^l]`.
    max_post: Vec<Vec<u32>>,
}

struct DomEntry<T> {
    pre: u32,
    post: u32,
    /// This node's fact met with every fact whose node dominates it.
    met: T,
}

impl<T: Copy> DomFacts<T> {
    /// `facts`' nodes outside the tree are dropped: they dominate no node
    /// in it.
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
        // Every entry up to `end` starts at or before `pre`, so it encloses the
        // query iff it has not finished before `post`; the last such is the
        // nearest dominating fact.
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

    // A value bounded on EVERY control-predecessor edge of a merge is bounded
    // at the merge by the union, though no single guard's edge dominates past
    // it.  One level: a predecessor that is not an `If` bounds nothing and
    // blocks the join.
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
            // Meets are taken in another order than the guards nest, which a
            // power-of-two stride makes associative.
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
    let (guarded, n) = crate::const_eval::var_and_const(function, function.producer(cmp_value))?;
    // Const on RHS means `v < N`; const on LHS means `N < v`.
    let const_on_rhs = function.producer_inputs_exact::<2>(cmp_value).ok()?[0] == guarded;

    let ty = function.value_type_opt(guarded)?;
    // An `IntCmpOp` operand is an integer per the node signature; the mask is
    // `None` only past 128 bits, where no `u128` is a bound the value is
    // inside.
    debug_assert!(ty.is_integer());
    let type_mask = ty.int_mask_u128()?;
    if ty == ValueType::I1 {
        return None;
    }
    // `Sless` bounds an unsigned range only where KnownBits proves the value
    // non-negative.
    let sign_bit = (type_mask >> 1) + 1;
    if op == IntCmpOp::Sless && known[guarded].zeros & sign_bit == 0 {
        return None;
    }
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

/// The sink of `edge` when it is reached EXCLUSIVELY via that edge, so a fact
/// taking the edge proves holds there. A `Region` still consuming an edge is a
/// genuine merge, where the fact does not hold for its other predecessors.
fn guard_sink(function: &strider_ir::Function, edge: ValueId) -> Option<NodeId> {
    single_control_consumer(function, edge)
        .filter(|&sink| !matches!(function.node_kind(sink), NodeKind::Region))
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
