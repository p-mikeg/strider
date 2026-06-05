//! Dominator-scoped per-`(value, region)` integer range analysis.
//!
//! Minimal interval lattice seeded from `If(IntCmp(v, const))` guards
//! (edge-sensitive, propagated via the control dominator tree) and from
//! `KnownBits` upper bounds.  Fail-closed: anything uncertain returns the
//! full type range (top).

use cranelift_entity::SecondaryMap;
use rustc_hash::FxHashMap;

use strider_ir::node::{NodeId, NodeKind, ValueId, ValueKind, ValueType};
use strider_ir::{IRViewer, IRWalker, IntBinaryOp, IntCmpOp, dominates};
use petgraph::algo::dominators::Dominators;

use crate::known_bits::{KnownBitsMap, KnownBitsFacts};

#[cfg(test)]
mod tests;

// ── Interval ─────────────────────────────────────────────────────────────────

/// Inclusive unsigned interval `[lo, hi]` over a value's bit width.
///
/// `lo` and `hi` are unsigned (u128) regardless of whether the value's type
/// is a signed integer.  The guard-extraction logic gates `Sless` on
/// KnownBits sign-bit = 0, so the interval is always non-negative.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Interval {
    pub lo: u128,
    pub hi: u128,
}

impl Interval {
    /// The top element: the full range `[0, width_mask]`.
    pub fn top(width_mask: u128) -> Self {
        Self {
            lo: 0,
            hi: width_mask,
        }
    }

    /// Returns `true` if this interval covers the full range for `width_mask`.
    pub fn is_top(&self, width_mask: u128) -> bool {
        self.lo == 0 && self.hi >= width_mask
    }

    /// Exclusive upper bound — the "entry count" a table index may reach —
    /// or `None` if this interval is top (unbounded).
    pub fn upper_exclusive(&self, width_mask: u128) -> Option<u64> {
        if self.hi >= width_mask {
            None
        } else {
            u64::try_from(self.hi + 1).ok()
        }
    }

    fn intersect(self, other: Self) -> Self {
        Self {
            lo: self.lo.max(other.lo),
            hi: self.hi.min(other.hi),
        }
    }

    fn union(self, other: Self) -> Self {
        Self {
            lo: self.lo.min(other.lo),
            hi: self.hi.max(other.hi),
        }
    }
}

// ── RangeMap ─────────────────────────────────────────────────────────────────

/// Result of `compute_value_ranges`.
///
/// Query via `range_of(value, region)`.
pub struct RangeMap<'f> {
    /// Reference to the function, needed for phi-chasing / type queries.
    function: &'f strider_ir::Function,
    /// Dominator tree, kept for query-time dominance checks.
    doms: &'f Dominators<NodeId>,
    /// Guard facts: for each guarded value, the list of
    /// `(true_succ_region, interval)` pairs recorded from `If` guards.
    /// Dominance is checked lazily at `range_of` query time — only guards
    /// whose `true_succ_region` dominates the query region apply.
    guards: FxHashMap<ValueId, Vec<(NodeId, Interval)>>,
    /// Flow-insensitive KnownBits upper bounds: for each value, the max
    /// value proven by the KnownBits analysis.  This is `[0, max_value]`
    /// derived from `KnownBitsFacts::max_value(type_mask)`.
    kb_bounds: SecondaryMap<ValueId, Option<Interval>>,
}

impl<'f> RangeMap<'f> {
    /// Range of `value` valid within `region`.
    ///
    /// Resolution order:
    /// 1. Chase trivial (single-data-input) phis to their underlying value.
    /// 2. For non-trivial phis: union each arm's range in the predecessor
    ///    region; if ANY arm is top, the result is top (fail-closed).
    /// 3. For the resolved value: intersect the guard fact with the
    ///    KnownBits base; if neither constrains, return top.
    pub fn range_of(&self, value: ValueId, region: NodeId) -> Interval {
        self.range_of_depth(value, region, 0)
    }

    fn range_of_depth(&self, value: ValueId, region: NodeId, depth: usize) -> Interval {
        const MAX_DEPTH: usize = 16;
        if depth > MAX_DEPTH {
            // Cycle or deeply nested phi — treat as top (fail-closed).
            let ty = self.function.value_kind(value).as_value();
            return Interval::top(ty.map_or(u128::MAX, |t| t.bit_mask_u128()));
        }

        let producer = self.function.producer(value);
        let kind = *self.function.node_kind(producer);

        // Chase Phi nodes.
        if matches!(kind, NodeKind::Phi) {
            return self.resolve_phi(producer, value, region, depth);
        }

        // Not a phi: look up guard + KB intersection for the bare value.
        self.resolve_leaf(value, region)
    }

    /// Resolve a `Phi` node: trivial single-data-input → recurse on the
    /// underlying value; multi-input → union of each arm's range in its
    /// predecessor region, fail-closed on any top arm.
    fn resolve_phi(
        &self,
        phi_node: NodeId,
        _phi_value: ValueId,
        region: NodeId,
        depth: usize,
    ) -> Interval {
        // A Phi node's inputs are: [PhiToken, v0, v1, …]
        // where v0, v1, … are the data inputs (one per predecessor).
        // The PhiToken is at index 0 and has kind PhiToken (not a value).
        let g = self.function.graph();
        let all_inputs: Vec<ValueId> = g.node_inputs(phi_node).iter().collect();

        // Separate data inputs (non-PhiToken) from the structural PhiToken.
        let data_inputs: Vec<ValueId> = all_inputs
            .iter()
            .copied()
            .filter(|&v| g.value_kind(v) != ValueKind::PhiToken)
            .collect();

        let ty = {
            let phi_outputs = g.node_outputs(phi_node);
            if phi_outputs.is_empty() {
                return Interval::top(u128::MAX);
            }
            self.function.value_kind(phi_outputs[0]).as_value()
        };
        let type_mask = ty.map_or(u128::MAX, |t| t.bit_mask_u128());

        if data_inputs.is_empty() {
            return Interval::top(type_mask);
        }

        // Trivial single-data-input phi → chase to the underlying value
        // in the SAME region (the phi is a transparent pass-through).
        if data_inputs.len() == 1 {
            return self.range_of_depth(data_inputs[0], region, depth + 1);
        }

        // Multi-input phi: find the predecessor regions.
        // The Region node's control inputs correspond 1-to-1 with the Phi's
        // data inputs (same positional order, after the PhiToken).
        //
        // Strategy: for each data input, determine its predecessor region
        // by finding the Region whose control is the corresponding input to
        // the joining Region node.  We identify the joining Region by finding
        // the Region that owns the Phi (i.e. the Region whose PhiToken flows
        // into this Phi's slot-0).
        let joining_region = self.find_joining_region(phi_node);
        if joining_region.is_none() {
            return Interval::top(type_mask);
        }
        let joining_region = joining_region.unwrap();

        // Get the predecessor regions in positional order.
        let pred_regions = self.predecessor_regions_of(joining_region);
        if pred_regions.len() != data_inputs.len() {
            // Mismatch — bail safely.
            return Interval::top(type_mask);
        }

        // Union the ranges for each arm; fail-closed on any top arm.
        let mut result: Option<Interval> = None;
        for (pred_region, &arm_val) in pred_regions.iter().zip(data_inputs.iter()) {
            let arm_range = self.range_of_depth(arm_val, *pred_region, depth + 1);
            if arm_range.is_top(type_mask) {
                return Interval::top(type_mask);
            }
            result = Some(match result {
                None => arm_range,
                Some(acc) => acc.union(arm_range),
            });
        }

        result.unwrap_or(Interval::top(type_mask))
    }

    /// Given a `Phi` node, find the `Region` node whose PhiToken is this
    /// Phi's first input (slot 0).
    fn find_joining_region(&self, phi_node: NodeId) -> Option<NodeId> {
        let g = self.function.graph();
        // Phi's slot 0 is the PhiToken.
        let phi_token_val = g.nth_input(phi_node, 0)?;
        if g.value_kind(phi_token_val) != ValueKind::PhiToken {
            return None;
        }
        let region_node = g.producer(phi_token_val);
        if matches!(g.node_kind(region_node), NodeKind::Region) {
            Some(region_node)
        } else {
            None
        }
    }

    /// Returns the predecessor Region nodes of `region` in the same
    /// positional order as the Region's control inputs.
    ///
    /// Each control input to the `Region` is the control output of some
    /// predecessor node (typically an `If` output or another region's
    /// branch output).  We trace back to the dominating `Region` /
    /// `Entry` node.
    fn predecessor_regions_of(&self, region: NodeId) -> Vec<NodeId> {
        let g = self.function.graph();
        let inputs: Vec<ValueId> = g.node_inputs(region).iter().collect();
        // Region's inputs are: [ctrl0, ctrl1, …] — all Control-typed.
        inputs
            .into_iter()
            .filter(|&v| g.value_kind(v).is_control())
            .map(|ctrl_val| {
                // Walk back through If outputs to find the predecessor Region.
                self.ctrl_source_region(ctrl_val)
            })
            .collect()
    }

    /// Given a control value, walk back to find the `Region` or `Entry`
    /// node it ultimately comes from.  This handles the case where the
    /// control passes through an `If` output (the `If`'s producer is not
    /// a Region, but its predecessor is).
    fn ctrl_source_region(&self, ctrl_val: ValueId) -> NodeId {
        let g = self.function.graph();
        let mut curr = g.producer(ctrl_val);
        // Walk through If/Call/CallOther nodes back to the nearest Region.
        // Any node kind that is not a branching control-consumer stops the
        // walk and returns `curr` directly.
        loop {
            match g.node_kind(curr) {
                NodeKind::If | NodeKind::Call | NodeKind::CallOther { .. } => {
                    // The controlling predecessor is the first input.
                    if let Some(pred_ctrl) = g.nth_input(curr, 0) {
                        curr = g.producer(pred_ctrl);
                    } else {
                        return curr;
                    }
                }
                _ => return curr,
            }
        }
    }

    /// Resolve the range of a leaf (non-Phi) value in `region`:
    /// intersect all dominating guard facts with the KB bound, or return top.
    ///
    /// Guard facts are stored lazily per-value (not per-(value, region)).
    /// We iterate the guards on `value` and intersect those whose
    /// `true_succ_region` dominates `region`.  This makes each query
    /// O(guards_on_v × domtree-depth) rather than requiring eager
    /// enumeration of all dominated regions at build time.
    fn resolve_leaf(&self, value: ValueId, region: NodeId) -> Interval {
        let ty = self.function.value_kind(value).as_value();
        let type_mask = ty.map_or(u128::MAX, |t| t.bit_mask_u128());

        // Collect the intersection of all guards that dominate `region`.
        let guard = if let Some(guard_list) = self.guards.get(&value) {
            guard_list
                .iter()
                .filter(|(guard_region, _)| dominates(self.doms, *guard_region, region))
                .map(|(_, interval)| *interval)
                .reduce(|acc, iv| acc.intersect(iv))
        } else {
            None
        };

        let kb = self.kb_bounds[value];

        match (guard, kb) {
            (Some(g), Some(k)) => g.intersect(k),
            (Some(g), None) => g,
            (None, Some(k)) => k,
            (None, None) => Interval::top(type_mask),
        }
    }
}

// ── Guard extraction helpers ──────────────────────────────────────────────────

/// Returns the all-ones bitmask for `ty` as a `u64`, capped at 64 bits.
/// Returns `None` for non-integer types or types wider than 64 bits.
fn type_mask_u64(ty: ValueType) -> Option<u64> {
    if !ty.is_integer() || !ty.fits_u64() {
        return None;
    }
    u64::try_from(ty.bit_mask_u128()).ok()
}

/// Returns `true` when KnownBits proves the sign bit of `value` is
/// zero (i.e. the value is always non-negative in signed interpretation).
fn is_sign_bit_known_zero(
    function: &strider_ir::Function,
    value: ValueId,
    known: &KnownBitsMap,
) -> bool {
    let Some(ty) = function.value_kind(value).as_value() else {
        return false;
    };
    let Some(type_mask) = type_mask_u64(ty) else {
        return false;
    };
    let sign_bit = (type_mask >> 1) + 1;
    known[value].zeros & sign_bit != 0
}


// ── Public entry point ────────────────────────────────────────────────────────

/// Computes the dominator-scoped integer range analysis for all values in
/// `function`.
///
/// Returns a [`RangeMap`] that answers `range_of(value, region)` queries.
///
/// Algorithm:
///
/// 1. **KnownBits base (flow-insensitive):** for each value with KnownBits
///    facts, derive `[0, max_value]`.
/// 2. **Guard facts (edge-sensitive, O(I)):** scan all `If` nodes.  For each
///    recognised condition shape, extract the single guarded `(ValueId, Interval)`
///    directly from the condition node — no scan over all values or all regions.
///    Store `(true_succ_region, interval)` lazily per value.
/// 3. **`range_of`:** resolve through trivial phis; intersect all dominating
///    guard facts with the KB bound (dominance checked at query time); default
///    to top.
pub fn compute_value_ranges<'f>(
    function: &'f strider_ir::Function,
    doms: &'f Dominators<NodeId>,
    known: &KnownBitsMap,
) -> RangeMap<'f> {
    // ── Step 1: KnownBits base ────────────────────────────────────────────
    let mut kb_bounds: SecondaryMap<ValueId, Option<Interval>> = SecondaryMap::new();
    for value_id in function.graph().all_value_ids() {
        let kb: KnownBitsFacts = known[value_id];
        // Skip fully unknown entries (the default).
        if kb.ones == 0 && kb.zeros == 0 {
            continue;
        }
        let Some(ty) = function.value_kind(value_id).as_value() else {
            continue;
        };
        let Some(type_mask_u64) = type_mask_u64(ty) else {
            continue;
        };
        let max_val = kb.max_value(type_mask_u64) as u128;
        let type_mask_u128 = ty.bit_mask_u128();
        // Only record if strictly tighter than the full range.
        if max_val < type_mask_u128 {
            kb_bounds[value_id] = Some(Interval { lo: 0, hi: max_val });
        }
    }

    // ── Step 2: Guard facts (O(I)) ────────────────────────────────────────
    //
    // For each `If` node: read its condition, shape-match ONCE to get the
    // guarded `(ValueId, Interval)` directly from the condition node's
    // operands.  Store `(true_succ_region, interval)` per value, lazily —
    // dominance is checked at query time, not enumerated here.
    let mut guards: FxHashMap<ValueId, Vec<(NodeId, Interval)>> = FxHashMap::default();

    for if_node in function
        .walk()
        .filter(|&n| matches!(function.node_kind(n), NodeKind::If))
    {
        // If's outputs: [true_ctrl, false_ctrl].  We only handle the true edge.
        let if_outputs = function.node_outputs(if_node);
        if if_outputs.len() < 2 {
            continue;
        }
        let true_ctrl = if_outputs[0];

        // Find the true-successor Region: the Region consuming true_ctrl.
        let Some(true_succ_region) = find_region_consuming(function, true_ctrl) else {
            continue;
        };

        // The condition value is If's input[1].
        let if_inputs: Vec<ValueId> = function.node_inputs(if_node).iter().collect();
        if if_inputs.len() < 2 {
            continue;
        }
        let cond_value = if_inputs[1];

        // Shape-match the condition ONCE to extract the guarded (value, interval).
        // Returns None if the shape is not recognised.
        let Some((guarded_value, guard_interval)) =
            extract_guard_from_condition(function, cond_value, known)
        else {
            continue;
        };

        // Record lazily: just push the (true_succ_region, interval) pair.
        // Dominance is checked at query time in resolve_leaf.
        guards
            .entry(guarded_value)
            .or_default()
            .push((true_succ_region, guard_interval));
    }

    RangeMap {
        function,
        doms,
        guards,
        kb_bounds,
    }
}

/// Shape-matches an `If` condition and returns the `(guarded_value, interval)`
/// pair it constrains on the true edge, or `None` if the shape is unrecognised.
///
/// Recognised shapes (same as before):
///
/// - Shape 1 (lowered `<=`): `Xor(IntCmpOp::Less(IntConst(N), value), IntConst(1)):I1`
///   → `value ∈ [0, N]`
/// - Shape 2 (strict `<`): `IntCmpOp::Less(value, IntConst(N))`
///   → `value ∈ [0, N-1]` (returns `None` when N == 0)
/// - Shape 2 signed: `IntCmpOp::Sless(value, IntConst(N))` with sign-bit known 0
///   → `value ∈ [0, N-1]` (returns `None` when N == 0)
///
/// Extraction is O(1): the guarded value is read directly from the condition
/// node's operands — no scan over all graph values.
fn extract_guard_from_condition(
    function: &strider_ir::Function,
    cond_value: ValueId,
    known: &KnownBitsMap,
) -> Option<(ValueId, Interval)> {
    let g = function.graph();
    let cond_producer = g.producer(cond_value);
    let cond_kind = *g.node_kind(cond_producer);

    // Shape 1: Xor(IntCmpOp::Less(IntConst(N), v), IntConst(1)):I1
    // This is the lowered form of `v <= N`.
    if let NodeKind::IntBinaryOp(IntBinaryOp::Xor) = cond_kind {
        let [xor_lhs, xor_rhs] = g.node_inputs_exact::<2>(cond_producer).ok()?;
        if function.int_const_u128(xor_rhs)? != 1 {
            return None;
        }
        let inner_producer = g.producer(xor_lhs);
        let inner_kind = *g.node_kind(inner_producer);
        if let NodeKind::IntCmpOp(op @ (IntCmpOp::Less | IntCmpOp::Sless)) = inner_kind {
            let [inner_lhs, inner_rhs] = g.node_inputs_exact::<2>(inner_producer).ok()?;
            // inner_lhs is IntConst(N), inner_rhs is the guarded value.
            let n = function.int_const_u128(inner_lhs)?;
            let guarded = inner_rhs;
            // Skip constants: they don't benefit from range bounds.
            if matches!(g.node_kind(g.producer(guarded)), NodeKind::IntConst(_)) {
                return None;
            }
            // For Sless: require sign bit known zero.
            if op == IntCmpOp::Sless
                && !is_sign_bit_known_zero(function, guarded, known)
            {
                return None;
            }
            // Skip booleans (I1) and non-integer types.
            let ty = function.value_kind(guarded).as_value()?;
            if !ty.is_integer() || ty == ValueType::I1 {
                return None;
            }
            let type_mask = ty.bit_mask_u128();
            // NOT (N < v) = (v <= N) → v ∈ [0, N].
            return Some((guarded, Interval { lo: 0, hi: n.min(type_mask) }));
        }
        return None;
    }

    // Shape 2: IntCmpOp::Less(value, IntConst(N)) or Sless with known-zero sign.
    if let NodeKind::IntCmpOp(op @ (IntCmpOp::Less | IntCmpOp::Sless)) = cond_kind {
        let [lhs, rhs] = g.node_inputs_exact::<2>(cond_producer).ok()?;
        let guarded = lhs;
        // Skip constants.
        if matches!(g.node_kind(g.producer(guarded)), NodeKind::IntConst(_)) {
            return None;
        }
        let n = function.int_const_u128(rhs)?;
        // v < 0 is impossible on an unsigned domain — return None (top).
        if n == 0 {
            return None;
        }
        if op == IntCmpOp::Sless && !is_sign_bit_known_zero(function, guarded, known) {
            return None;
        }
        // Skip booleans (I1) and non-integer types.
        let ty = function.value_kind(guarded).as_value()?;
        if !ty.is_integer() || ty == ValueType::I1 {
            return None;
        }
        let type_mask = ty.bit_mask_u128();
        // idx < N → idx ∈ [0, N-1].
        return Some((guarded, Interval {
            lo: 0,
            hi: n.saturating_sub(1).min(type_mask),
        }));
    }

    None
}

/// Find the `Region` node that consumes `ctrl_val` as a control input.
fn find_region_consuming(function: &strider_ir::Function, ctrl_val: ValueId) -> Option<NodeId> {
    let g = function.graph();
    for (consumer, _slot) in g.value_uses(ctrl_val) {
        if matches!(g.node_kind(consumer), NodeKind::Region) {
            return Some(consumer);
        }
    }
    None
}
