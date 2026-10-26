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

mod bit_field;
mod interval;
mod trust;

#[cfg(test)]
mod tests;

pub use interval::Interval;

use bit_field::{BitField, MaskOperands, bit_field};
use interval::{Hop, ScaleOp, type_mask_or_top, width_mask};

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
    /// What each `And` with a constant mask is the mask of.
    masks: MaskOperands,
    /// The guarded values whose [`BitField`] is exactly this one.
    guards_by_field: FxHashMap<BitField, Vec<ValueId>>,
    /// The distinct keys of `guards_by_field`, by root.
    fields_by_root: FxHashMap<ValueId, Vec<BitField>>,
    /// [`Self::bit_field`] answers, computed on first ask.
    field_memo: std::cell::RefCell<SecondaryMap<ValueId, Option<Option<BitField>>>>,
    /// Open `range_of` frames, against [`MAX_RANGE_DEPTH`].
    depth: u32,
    /// Whether the query in flight took a depth or cycle cut. Its frames fold
    /// the cut's `top` into answers the key does not record the cut in, so
    /// the memo is dropped rather than handed to the next query.
    cut: bool,
    /// Every `If`.
    ifs: Vec<NodeId>,
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
            self.resolve_phi(producer, region)
        } else if let Some(operand) = self.masks.identity[value] {
            let leaf = self.resolve_leaf(value, region);
            self.range_of(operand, region).intersect(leaf)
        } else {
            let leaf = self.resolve_leaf(value, region);
            match self.const_scale(value) {
                Some((operand, scale)) => match self.dominating_guard(operand, region) {
                    Some(guard) => {
                        let width_mask = width_mask(self.function, value);
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
        if self.depth == 0 && std::mem::take(&mut self.cut) {
            self.memo.clear();
        }
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

    /// The distinct fields of `root` some guarded value is.
    fn root_fields(&self, root: ValueId) -> &[BitField] {
        self.fields_by_root.get(&root).map_or(&[], Vec::as_slice)
    }

    /// `value` as bits of the value it is extracted from; see [`BitField`].
    fn bit_field(&self, value: ValueId) -> Option<BitField> {
        if let Some(hit) = self.field_memo.borrow()[value] {
            return hit;
        }
        let field = bit_field(self.function, &self.masks, value);
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
        if !self.doms.dominates(joining, region) {
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
    ///
    /// A zero-extend is the identity on the value, so the compared value
    /// stands for each value it zero-extends: a guard on `zext32(b)` bounds
    /// an arm `zext64(b)`.
    fn edge_bound_on(&self, edge: ValueId, arm: ValueId) -> Option<Interval> {
        let (guarded, iv) = *self.edge_guards.get(&edge)?;
        let f = self.function;
        let mut same = vec![guarded];
        while same.len() < MAX_GUARD_LADDER as usize {
            let last = same[same.len() - 1];
            if !matches!(
                f.node_kind(f.producer(last)),
                NodeKind::Extend(ExtendOp::ZeroExtend)
            ) {
                break;
            }
            let Ok([x]) = f.producer_inputs_exact::<1>(last) else {
                break;
            };
            same.push(x);
        }
        let mut hops: Vec<Hop> = Vec::new();
        let mut cur = arm;
        for _ in 0..MAX_GUARD_LADDER {
            if same.contains(&cur) {
                break;
            }
            let (operand, hop) = self.arm_carrying_operand(cur)?;
            hops.push(hop);
            cur = operand;
        }
        if same.contains(&cur) {
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
        let mask = width_mask(self.function, value);
        Some((operand, Hop::Scaled(scale, mask)))
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

    /// The `Region` whose PhiToken this Phi takes.
    fn find_joining_region(&self, phi_node: NodeId) -> Option<NodeId> {
        let f = self.function;
        let phi_token_val = f.nth_input(phi_node, NodeKind::PHI_TOKEN_SLOT)?;
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
                if matches!(g.node_kind(producer), NodeKind::If) {
                    let if_outputs = g.node_outputs(producer);
                    if if_outputs.get(NodeKind::IF_TRUE_OUT_SLOT) == Some(&ctrl_val) {
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
                    if let Some(pred_ctrl) = f.nth_input(curr, NodeKind::CTRL_SLOT) {
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
    /// Guards met by a lookup, one per guard dominating the query point: the
    /// cost a query pays, which must track neither the chain's depth nor the
    /// value's guard count.
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
        if *function.kind_of_value(v) != NodeKind::IntBinaryOp(op) {
            return None;
        }
        let [x, k] = function.producer_inputs_exact::<2>(v).ok()?;
        Some((x, u32::try_from(function.int_const_u128(k)?).ok()?))
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
    let mut masks = MaskOperands::default();
    for node in function.walk() {
        masks.record(function, node, known);
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

            // Back-propagate through `Add(X, const)`, where a guard on `idx - K`
            // gives `idx` the shifted bound, and through a rotate.
            // Bounded like every other walk here: an `Add` spine is linear in
            // the function.
            const MAX_ADD_BACKPROP: usize = 64;
            let mut cur = guarded_value;
            let mut iv = guard_interval;
            let mut steps = 0usize;
            while let Some((operand, shifted)) = add_operand_shifted_interval(function, cur, iv)
                .or_else(|| rotate_operand_interval(function, cur, iv))
            {
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
    }

    let guard_forests = guards
        .iter()
        .map(|(&value, by_node)| (value, GuardForest::new(&doms, by_node)))
        .collect();
    let mut guards_by_field: FxHashMap<BitField, Vec<ValueId>> = FxHashMap::default();
    let mut fields_by_root: FxHashMap<ValueId, Vec<BitField>> = FxHashMap::default();
    for &value in guards.keys() {
        if let Some(field) = bit_field(function, &masks, value) {
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
        masks,
        guards_by_field,
        fields_by_root,
        field_memo: std::cell::RefCell::new(SecondaryMap::new()),
        depth: 0,
        cut: false,
        ifs,
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
