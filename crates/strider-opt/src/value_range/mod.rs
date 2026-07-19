//! Dominator-scoped per-`(value, region)` integer range analysis.
//!
//! Minimal interval lattice seeded from `If(IntCmp(v, const))` guards
//! (edge-sensitive, propagated via the control dominator tree) and from
//! `KnownBits` upper bounds.  Fail-closed: anything uncertain returns the
//! full type range (top).
//!
//! SOUNDNESS INVARIANT: `range_of` may widen toward top but must NEVER return
//! an interval tighter than the real runtime value set.  The jump-table
//! classifier enumerates `lo..=hi` as the COMPLETE target set, so a too-tight
//! bound silently drops real branch targets.  Any future refinement that
//! intersects a non-dominating fact, or otherwise tightens an interval, has to
//! re-justify this.

use cranelift_entity::SecondaryMap;
use rustc_hash::FxHashMap;

use petgraph::algo::dominators::Dominators;
use strider_ir::node::{NodeId, NodeKind, ValueId, ValueKind, ValueType};
use strider_ir::{IRViewer, IRWalker, IntBinaryOp, IntCmpOp, dominates};

use crate::opt::known_bits::{KnownBitsFacts, KnownBitsMap};

#[cfg(test)]
mod tests;

/// The inclusive value set `{ lo, lo+stride, lo+2*stride, ... hi }`.
///
/// `lo`/`hi` are unsigned regardless of signedness: guard extraction gates
/// `Sless` on KnownBits sign-bit = 0, so the interval is non-negative.
///
/// `stride` is a must-congruence from KnownBits (low `k` bits known-zero means
/// a multiple of `2^k`).  It is always a sound divisor of the real spacing, so
/// [`Self::count`] never under-counts.  That lets a scaled index
/// `idx*8 = [0, 4800, stride 8]` read as 601 entries, not a 4800-wide span.
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

    pub fn is_top(&self, width_mask: u128) -> bool {
        self.lo == 0 && self.hi >= width_mask
    }

    /// What the table-dispatch classifier enumerates and caps.  `0` if `hi < lo`.
    pub fn count(&self) -> u128 {
        if self.hi < self.lo {
            0
        } else {
            (self.hi - self.lo) / self.stride.max(1) + 1
        }
    }

    /// `None` when top.  Production reads `hi`/`lo` directly.
    #[cfg(test)]
    pub fn upper_exclusive(&self, width_mask: u128) -> Option<u64> {
        if self.hi >= width_mask {
            None
        } else {
            u64::try_from(self.hi + 1).ok()
        }
    }

    fn intersect(self, other: Self) -> Self {
        // Keep a stride only when one side is dense, which covers the common
        // guard-meets-KnownBits case (a guard is stride 1, so the KnownBits
        // stride survives).  A genuine two-stride meet would need alignment
        // reasoning, so fall back to 1 and enumerate everything.
        let stride = if self.stride == 1 {
            other.stride
        } else if other.stride == 1 {
            self.stride
        } else {
            1
        };
        Self {
            lo: self.lo.max(other.lo),
            hi: self.hi.min(other.hi),
            stride,
        }
    }

    fn union(self, other: Self) -> Self {
        Self {
            lo: self.lo.min(other.lo),
            hi: self.hi.max(other.hi),
            stride: 1, // sound over-approximation for a join
        }
    }
}

/// Three-colour cycle marking for a `(value, region)` query: absent is white,
/// `InProgress` grey, `Done` black.
#[derive(Clone, Copy)]
enum MemoSlot {
    /// Re-entry means a dependency cycle (a loop-carried phi-of-phi).  The
    /// back-edge resolves to top, matching what any fixpoint's first iteration
    /// would give, without recursing forever.
    InProgress,
    Done(Interval),
}

/// Result of `compute_value_ranges`; query via `range_of(value, region)`.
pub struct RangeMap<'f> {
    function: &'f strider_ir::Function,
    doms: &'f Dominators<NodeId>,
    /// Per guarded value, the `(guard_node, interval)` pairs from `If` guards.
    /// `guard_node` is the unique control consumer of the guarded edge: a
    /// `Region` before `RegionCollapse`, any other control node after it
    /// deletes the dispatch Region.  Dominance is checked lazily at query
    /// time, so only guards dominating the query node apply.
    guards: FxHashMap<ValueId, Vec<(NodeId, Interval)>>,
    /// Flow-insensitive `[0, max_value]` bounds from KnownBits.
    kb_bounds: SecondaryMap<ValueId, Option<Interval>>,
    /// Caches resolved intervals so a value shared across phi arms resolves
    /// once, and cuts resolution cycles via `InProgress` instead of an
    /// arbitrary recursion-depth cap.  Persists across queries, so successive
    /// anchor/candidate lookups share resolved arms.
    memo: FxHashMap<(ValueId, NodeId), MemoSlot>,
}

impl<'f> RangeMap<'f> {
    /// Range of `value` valid within `region`.
    ///
    /// A multi-input phi unions each arm's range in that arm's predecessor
    /// region, falling back to a guard on the phi's own output if any arm is
    /// top.  Any other value intersects its dominating guards with the
    /// KnownBits base.  Either way, unconstrained means top.
    ///
    /// A single-input phi cannot occur in the converged IR this runs on
    /// (`PhiCollapse` removed it), so a degenerate phi resolves to top.
    pub fn range_of(&mut self, value: ValueId, region: NodeId) -> Interval {
        let key = (value, region);

        // A grey hit is a resolution cycle (a loop-carried phi-of-phi), cut by
        // returning top for the back-edge.  The frame that opened the cycle
        // still applies any dominating guard on its way out, so a bounded loop
        // index keeps its bound and an unbounded one stays top.
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

        // Bounds are deliberately not propagated UP a `ZeroExtend`/`Truncate`
        // chain such as `ZeroExtend(Truncate(rdi))` guarded on the inner
        // truncate.  The only consumer, the jump-table classifier's
        // `candidate_range` scan, already reaches the inner guarded node.
        let result = if matches!(self.function.node_kind(producer), NodeKind::Phi) {
            self.resolve_phi(producer, region)
        } else {
            self.resolve_leaf(value, region)
        };

        self.memo.insert(key, MemoSlot::Done(result));
        result
    }

    /// `region` is consulted only for a guard recorded against the phi's own
    /// output; the per-arm ranges use the effective regions derived from the
    /// joining region.
    fn resolve_phi(&mut self, phi_node: NodeId, region: NodeId) -> Interval {
        let data_inputs: Vec<ValueId> = self.function.phi_data_inputs(phi_node).collect();

        // Empty outputs means a degenerate phi: top.
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

        // Single-input phis cannot reach here: `PhiCollapse` eliminated them in
        // the converged IR this runs on.
        if data_inputs.len() < 2 {
            return Interval::top(type_mask);
        }

        // The Region's control inputs line up 1-to-1 with the phi's data
        // inputs, so each arm is queried in its own effective region (see
        // `arm_query_regions`).  That is what keeps a guard holding on only one
        // incoming path from being applied to an arm that bypasses it.
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
                // bounds it at the query point whichever arm the value came
                // from.  Guards key on the phi directly, since a multi-input
                // phi is never chased through.
                return self
                    .dominating_guard(phi_value, region)
                    .unwrap_or(arm_range);
            }
            result = Some(result.map_or(arm_range, |acc| acc.union(arm_range)));
        }

        let union = result.expect("union has >= 1 arm by the guards above");
        // A guard holding at the query point is valid for every arm, so it
        // refines the union and recovers a bound the union alone loses.
        match self.dominating_guard(phi_value, region) {
            Some(guard) => union.intersect(guard),
            None => union,
        }
    }

    /// Intersection of every guard on `value` whose `guard_node` dominates
    /// `region`, or `None` when none apply.
    pub(crate) fn dominating_guard(&self, value: ValueId, region: NodeId) -> Option<Interval> {
        self.guards.get(&value).and_then(|guard_list| {
            guard_list
                .iter()
                .filter(|(guard_region, _)| dominates(self.doms, *guard_region, region))
                .map(|(_, interval)| *interval)
                .reduce(|acc, iv| acc.intersect(iv))
        })
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
    /// that arm.
    ///
    /// An arm arriving on an If's TRUE edge has the joining region as its own
    /// true-successor, and guards are stored as `(true_succ_region, iv)`, so
    /// querying in `joining_region` finds the guard by reflexive dominance.
    /// Every other arm (unconditional branch, false edge, other producer)
    /// traces back to its source Region, where only a guard dominating that
    /// predecessor applies.
    fn arm_query_regions(&self, joining_region: NodeId) -> Vec<NodeId> {
        let g = self.function.graph();
        // Every `Region` input is a Control edge per the node signature; the
        // filter just keeps arm positions aligned with the phi's value inputs
        // should that ever change.
        g.node_inputs(joining_region)
            .iter()
            .filter(|&v| g.value_kind(v).is_control())
            .map(|ctrl_val| {
                let producer = g.producer(ctrl_val);
                // An If's outputs are [true_ctrl, false_ctrl], so being the
                // first output identifies the true edge.
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

    /// Walks back through branching control consumers to the `Region` or
    /// `Entry` the control ultimately comes from.
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

    /// Guards are stored per-value, not per-(value, region), so a query costs
    /// O(guards_on_v * domtree-depth) instead of eagerly enumerating every
    /// dominated region at build time.
    fn resolve_leaf(&self, value: ValueId, region: NodeId) -> Interval {
        let ty = self.function.value_type_opt(value);
        let type_mask = type_mask_or_top(ty);

        let guard = self.dominating_guard(value, region);
        let kb = self.kb_bounds[value];

        // `intersect` is commutative, so the order here does not matter.
        guard
            .into_iter()
            .chain(kb)
            .reduce(Interval::intersect)
            .unwrap_or_else(|| Interval::top(type_mask))
    }
}

/// `u128::MAX` (unconstrained top) when the value carries no typed edge.
fn type_mask_or_top(ty: Option<ValueType>) -> u128 {
    ty.map_or(u128::MAX, |t| t.bit_mask_u128())
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
    // Exactly one operand must be the constant addend.
    let (operand, c) = match (function.int_const_u128(a), function.int_const_u128(b)) {
        (None, Some(c)) => (a, c),
        (Some(c), None) => (b, c),
        _ => return None,
    };
    let mask = function.value_type_opt(operand)?.bit_mask_u128();
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

/// Builds the flow-insensitive KnownBits bases, then scans every `If` to
/// extract guard facts keyed per value.  Dominance is resolved at query time
/// in [`RangeMap::range_of`], not enumerated here.
pub fn compute_value_ranges<'f>(
    function: &'f strider_ir::Function,
    doms: &'f Dominators<NodeId>,
    known: &KnownBitsMap,
) -> RangeMap<'f> {
    let mut kb_bounds: SecondaryMap<ValueId, Option<Interval>> = SecondaryMap::new();
    // Walk the live graph rather than the raw arena: a culled-but-not-compacted
    // value carries no useful KnownBits anyway, and O(reachable) <= O(arena).
    for node in function.walk() {
        for &value_id in function.node_outputs(node) {
            let kb: KnownBitsFacts = known[value_id];
            // Fully unknown is the default.
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
            // `k` trailing known-zero bits means a multiple of `2^k`, i.e. the
            // interval stride.  Capped to the range so it stays a valid divisor.
            let tz = kb.zeros.trailing_ones().min(127);
            let stride = (1u128 << tz).min(max_val.max(1));
            // Record only when strictly tighter than the full range.
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
        // Both edges are modelled: the condition implies an interval on the
        // true edge, its negation one on the false edge.
        let if_outputs = function.node_outputs(if_node);
        if if_outputs.len() < 2 {
            continue;
        }
        let true_ctrl = if_outputs[0];
        let false_ctrl = if_outputs[1];

        let cond_value = function.if_cond(if_node);

        // Keying the guard by the edge's unique control CONSUMER (not by a
        // Region) survives `RegionCollapse` deleting the dispatch Region: the
        // guard then keys on whatever control node consumes the edge.
        for (edge_ctrl, edge_taken) in [(true_ctrl, true), (false_ctrl, false)] {
            let Some((guarded_value, guard_interval)) =
                guard_from_compare(function, cond_value, edge_taken, known)
            else {
                continue;
            };

            // Soundness gate: attach the guard only where the consumer is
            // reached EXCLUSIVELY via this edge.  `RegionCollapse` has already
            // dissolved every single-predecessor `Region`, so a `Region` still
            // consuming an edge is a genuine merge and the guard does not hold
            // for its other predecessors.  Every other control consumer has
            // exactly one control input by signature.
            let Some(consumer) = single_control_consumer(function, edge_ctrl) else {
                continue;
            };
            if matches!(function.node_kind(consumer), NodeKind::Region) {
                continue;
            }

            // No trivial-phi chase needed: `PhiCollapse` already eliminated
            // single-input phis, so the guarded value is its own base.
            guards
                .entry(guarded_value)
                .or_default()
                .push((consumer, guard_interval));

            // Back-propagate through `Add(X, const)`: this is the offset-switch
            // shape, where the guard sits on `idx - K` while the dispatch
            // indexes `idx`, so `idx` inherits the shifted bound.
            let mut cur = guarded_value;
            let mut iv = guard_interval;
            while let Some((operand, shifted)) = add_operand_shifted_interval(function, cur, iv) {
                guards.entry(operand).or_default().push((consumer, shifted));
                cur = operand;
                iv = shifted;
            }
        }
    }

    RangeMap {
        function,
        doms,
        guards,
        kb_bounds,
        memo: FxHashMap::default(),
    }
}

/// The `(guarded_value, interval)` a bare `Less`/`Sless` condition constrains
/// on one edge, with `edge_taken` selecting true successor (condition holds)
/// or false (its negation).  `Sless` is gated on KnownBits sign-bit = 0.
///
/// Only upper-bounded intervals matter for table sizing, so a purely
/// lower-bounded constraint like `v >= N` returns `None` rather than being
/// recorded.
///
/// The lowered `<=` shape `Xor(Less(N, v), 1):I1` needs no handling: by the
/// time this runs, `IfCondInversion` has rewritten `If(Xor(C,1))` to `If(C)`
/// with swapped branches, so the condition is already a bare compare.
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
            // Nothing to bound; const-fold owns the both-const case.
            (Some(_), Some(_)) | (None, None) => return None,
            (None, Some(n)) => (lhs, n, true),
            (Some(n), None) => (rhs, n, false),
        };

    // The guarded operand must be an integer wider than a bool.
    let ty = function.value_type_opt(guarded)?;
    if !ty.is_integer() || ty == ValueType::I1 {
        return None;
    }
    if op == IntCmpOp::Sless && !is_sign_bit_known_zero(function, guarded, known) {
        return None;
    }
    let type_mask = ty.bit_mask_u128();

    //   const_on_rhs  edge_taken  meaning     bound
    //   true (v<N)    true        v < N       [0, N-1] for N>0
    //   true (v<N)    false       v >= N      lower-only
    //   false (N<v)   true        v >= N+1    lower-only
    //   false (N<v)   false       v <= N      [0, N]
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
        // Lower-only constraints carry no useful upper bound.
        (true, false) | (false, true) => None,
    }
}

/// Each control edge has a single sink, so the first consumer is the only one.
/// `None` for a dead edge.
fn single_control_consumer(function: &strider_ir::Function, ctrl_val: ValueId) -> Option<NodeId> {
    function
        .graph()
        .value_uses(ctrl_val)
        .map(|(consumer, _slot)| consumer)
        .next()
}
