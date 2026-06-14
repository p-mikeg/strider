//! Dominator-scoped per-`(value, region)` integer range analysis.
//!
//! Minimal interval lattice seeded from `If(IntCmp(v, const))` guards
//! (edge-sensitive, propagated via the control dominator tree) and from
//! `KnownBits` upper bounds.  Fail-closed: anything uncertain returns the
//! full type range (top).
//!
//! SOUNDNESS INVARIANT — `range_of` must only ever return a SOUND UPPER BOUND
//! on a value's true runtime range; it may over-approximate (widen toward top)
//! but must NEVER under-approximate (return an interval tighter than the real
//! set of runtime values).  The indirect-branch jump-table classifier
//! enumerates `lo..=hi` and treats the result as the COMPLETE target set, so a
//! too-tight bound there would silently drop real branch targets.  Every arm
//! below (KnownBits `max_value`, guard `[0, N-1]`, the exact `Add` back-prop,
//! the fail-closed Phi union) preserves this.  Any future refinement that
//! *intersects* a non-dominating fact or otherwise tightens an interval must
//! re-justify this invariant.

use cranelift_entity::SecondaryMap;
use rustc_hash::FxHashMap;

use petgraph::algo::dominators::Dominators;
use strider_ir::node::{NodeId, NodeKind, ValueId, ValueKind, ValueType};
use strider_ir::{IRViewer, IRWalker, IntBinaryOp, IntCmpOp, dominates};

use crate::opt::known_bits::{KnownBitsFacts, KnownBitsMap};

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

/// Memoisation slot for a `(value, region)` query.
///
/// The three states implement standard recursion-cycle colouring:
/// absent = white (never visited), [`MemoSlot::InProgress`] = grey (on the
/// current resolution stack — re-entering it is a cycle), [`MemoSlot::Done`]
/// = black (fully resolved, reuse the cached interval).
#[derive(Clone, Copy)]
enum MemoSlot {
    /// On the current resolution stack: re-entry is a dependency cycle (a
    /// loop-carried phi-of-phi).  The back-edge resolves to top — the same
    /// "give up on the cycle" the result would have under any fixpoint's
    /// first iteration — without the resolver ever recursing forever.
    InProgress,
    /// Fully resolved: the region-sensitive interval for this `(value, region)`.
    Done(Interval),
}

/// Result of `compute_value_ranges`.
///
/// Query via `range_of(value, region)`.
pub struct RangeMap<'f> {
    /// Reference to the function, needed for phi-chasing / type queries.
    function: &'f strider_ir::Function,
    /// Dominator tree, kept for query-time dominance checks.
    doms: &'f Dominators<NodeId>,
    /// Guard facts: for each guarded value, the list of `(guard_node, interval)`
    /// pairs recorded from `If` guards.  `guard_node` is the unique control
    /// consumer of the guarded If-edge — a `Region` before `RegionCollapse`,
    /// or any other control node (If/IndirectBranch/…) after it deletes the
    /// dispatch Region.  Dominance is checked lazily at `range_of` query time:
    /// only guards whose `guard_node` dominates the query node apply.
    guards: FxHashMap<ValueId, Vec<(NodeId, Interval)>>,
    /// Flow-insensitive KnownBits upper bounds: for each value, the max
    /// value proven by the KnownBits analysis.  This is `[0, max_value]`
    /// derived from `KnownBitsFacts::max_value(type_mask)`.
    kb_bounds: SecondaryMap<ValueId, Option<Interval>>,
    /// Per-`(value, region)` result memo.  Caches resolved intervals (so a
    /// value shared across phi arms is resolved once, not once per path) and
    /// — via the [`MemoSlot::InProgress`] marker — cuts resolution cycles
    /// instead of bounding them with an arbitrary recursion-depth cap.
    /// `range_of` takes `&mut self` to fill it; the memo persists across
    /// queries, so successive anchor/candidate lookups share resolved arms.
    memo: FxHashMap<(ValueId, NodeId), MemoSlot>,
}

impl<'f> RangeMap<'f> {
    /// Range of `value` valid within `region`.
    ///
    /// Memoised per `(value, region)`: a resolved interval is cached and
    /// reused, and a re-entry into a still-resolving `(value, region)` is a
    /// dependency cycle (a loop-carried phi-of-phi) cut by returning top.
    ///
    /// Resolution order for a fresh query:
    /// 1. A multi-input phi: union each arm's range in that arm's predecessor
    ///    region; if ANY arm is top, fall back to a guard on the phi's own
    ///    output, else top (fail-closed).  (A single-input phi can't occur in
    ///    the converged IR this runs on — `PhiCollapse` removed it — so a
    ///    degenerate <2-input phi resolves to top.)
    /// 2. Any other value: intersect every dominating guard fact with the
    ///    KnownBits base; if neither constrains, return top.
    pub fn range_of(&mut self, value: ValueId, region: NodeId) -> Interval {
        let key = (value, region);

        // Memo / cycle check (3-colour).  A cached result is reused; a
        // grey (`InProgress`) hit is a resolution cycle — a loop-carried
        // phi-of-phi referencing itself — which we cut by returning top for
        // the back-edge.  The frame that opened the cycle still applies any
        // dominating guard on its way out, so a bounded loop index keeps its
        // bound; an unbounded one stays top.
        match self.memo.get(&key) {
            Some(MemoSlot::Done(iv)) => return *iv,
            Some(MemoSlot::InProgress) => {
                let ty = self.function.value_type_opt(value);
                return Interval::top(ty.map_or(u128::MAX, |t| t.bit_mask_u128()));
            }
            None => {}
        }
        // Mark grey before recursing.
        self.memo.insert(key, MemoSlot::InProgress);

        let producer = self.function.producer(value);

        // Chase Phi nodes; every other value resolves to its own guard + KB.
        //
        // Note we do NOT propagate a bound *up* a `ZeroExtend`/`Truncate` cast
        // chain (e.g. the x64 index `ZeroExtend(Truncate(rdi))` whose guard sits
        // on the inner `Truncate(rdi)`).  The only consumer — the jump-table
        // classifier's `find_index_candidates` — already walks the whole dispatch
        // cone and tries the inner guarded node directly (substituting it folds
        // the dispatch), so bounding the outer cast is redundant.
        let result = if matches!(self.function.node_kind(producer), NodeKind::Phi) {
            self.resolve_phi(producer, region)
        } else {
            self.resolve_leaf(value, region)
        };

        // Mark black: overwrite the grey marker with the resolved interval.
        self.memo.insert(key, MemoSlot::Done(result));
        result
    }

    /// Resolve a multi-input `Phi`: union each arm's range in that arm's
    /// predecessor region, fail-closed on any top arm (falling back to a guard
    /// recorded on the phi's own output).  A degenerate <2-input phi — which
    /// `PhiCollapse` precludes in the converged IR — resolves to top.
    fn resolve_phi(&mut self, phi_node: NodeId, region: NodeId) -> Interval {
        // `region` is consulted only for a guard recorded against the phi's own
        // output (the fail-closed and final-intersect paths below); the per-arm
        // ranges use the effective query regions derived from the joining region.
        //
        // A Phi node's inputs are: [PhiToken, v0, v1, …]
        // where v0, v1, … are the data inputs (one per predecessor).
        // The PhiToken is at index 0 and has kind PhiToken (not a value).
        let g = self.function.graph();

        // Collect data inputs in one pass: filter out the structural PhiToken (slot 0).
        let data_inputs: Vec<ValueId> = self.function.phi_data_inputs(phi_node).collect();

        // A Phi has exactly one output — its value — so derive it here rather
        // than taking it as a (redundant) parameter.  Empty outputs ⇒ degenerate
        // phi, resolve to top.
        let Some(phi_value) = g.node_outputs(phi_node).first().copied() else {
            return Interval::top(u128::MAX);
        };
        let ty = self.function.value_type_opt(phi_value);
        let type_mask = ty.map_or(u128::MAX, |t| t.bit_mask_u128());

        // A real (multi-input) phi at a control merge.  Single-input phis don't
        // reach here: `PhiCollapse` has already eliminated them in the converged
        // IR this analysis runs on.  (A 0-input phi is degenerate → top.)
        if data_inputs.len() < 2 {
            return Interval::top(type_mask);
        }

        // Multi-input phi: query each arm in its per-arm effective region.
        //
        // The Region node's control inputs correspond 1-to-1 with the Phi's
        // data inputs (same positional order, after the PhiToken).
        //
        // For each arm `i`, the effective query region is determined by how
        // the control flows into that arm's slot:
        //
        // - If the joining Region's control input `i` is the TRUE output of an
        //   `If` node (i.e. the arm arrives via an If's true edge), then the
        //   true-successor IS the joining region and the guard `(joining_region, iv)`
        //   applies.  Query in `joining_region` so `dominates(joining_region,
        //   joining_region)` = true and the guard is found.
        //
        // - Otherwise (the arm arrives via an unconditional branch or a false
        //   edge), query in the predecessor Region obtained by tracing the
        //   control edge back through If/Call/CallOther to its source Region.
        //   Any guard whose `true_succ_region` dominates that predecessor
        //   applies; a guard that only holds on a sibling path does not.
        //
        // Soundness: this ensures that a guard touching only ONE incoming path
        // is never applied to an arm that bypasses the guard entirely.
        let Some(joining_region) = self.find_joining_region(phi_node) else {
            return Interval::top(type_mask);
        };

        // Collect the per-arm effective query regions.
        let arm_regions = self.arm_query_regions(joining_region);
        if arm_regions.len() != data_inputs.len() {
            // Arity mismatch — bail safely (fail-closed).
            return Interval::top(type_mask);
        }

        // Union the ranges for each arm; fail-closed on any top arm.
        let mut result: Option<Interval> = None;
        for (arm_region, &arm_val) in arm_regions.iter().zip(data_inputs.iter()) {
            let arm_range = self.range_of(arm_val, *arm_region);
            if arm_range.is_top(type_mask) {
                // The per-arm union is top — but a guard recorded against the
                // phi's OWN output (a multi-input phi is never chased through,
                // so guards key on it directly) can still bound it at the query
                // point regardless of which arm the value came from.
                return self
                    .dominating_guard(phi_value, region)
                    .unwrap_or(arm_range);
            }
            result = Some(match result {
                None => arm_range,
                Some(acc) => acc.union(arm_range),
            });
        }

        let union = result.unwrap_or_else(|| Interval::top(type_mask));
        // Intersect any dominating guard recorded on the phi output itself.  A
        // guard holding at the query point is valid for every arm, so it
        // refines the arm union (and recovers a bound the union alone loses).
        match self.dominating_guard(phi_value, region) {
            Some(guard) => union.intersect(guard),
            None => union,
        }
    }

    /// The intersection of all guard facts recorded on `value` whose
    /// `guard_node` dominates `region`, or `None` when none apply.
    ///
    /// Shared by [`Self::resolve_leaf`] and [`Self::resolve_phi`]: the
    /// dominance filter is identical, the only difference being that
    /// `resolve_leaf` falls back to the KnownBits base / top while
    /// `resolve_phi` intersects this into the per-arm union.
    pub(crate) fn dominating_guard(&self, value: ValueId, region: NodeId) -> Option<Interval> {
        self.guards.get(&value).and_then(|guard_list| {
            guard_list
                .iter()
                .filter(|(guard_region, _)| dominates(self.doms, *guard_region, region))
                .map(|(_, interval)| *interval)
                .reduce(|acc, iv| acc.intersect(iv))
        })
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

    /// Returns, for each control input of `joining_region`, the effective
    /// query region for that arm's value in the multi-input phi resolution.
    ///
    /// Two cases per arm (control input `i` of the joining region):
    ///
    /// 1. The control value is produced by an `If` node AND it is the If's
    ///    **true** output (slot 0 of the If's outputs).  In this case the arm
    ///    arrives via an If's true edge whose true-successor IS the joining
    ///    region.  Guard facts are stored as `(true_succ_region, iv)`.
    ///    `dominates(joining_region, joining_region)` is reflexively true, so
    ///    querying in `joining_region` finds the guard correctly.
    ///    → effective query region = `joining_region`.
    ///
    /// 2. Otherwise (unconditional branch, false edge, or other control
    ///    producer) → trace back to the source Region via
    ///    `ctrl_source_region`.  A guard only applies if its
    ///    `true_succ_region` dominates that predecessor Region.
    ///    → effective query region = predecessor Region.
    fn arm_query_regions(&self, joining_region: NodeId) -> Vec<NodeId> {
        let g = self.function.graph();
        // Every `Region` input is a Control edge (per the node signature), so
        // this filter is a no-op safeguard that keeps arm positions aligned
        // with the phi's value inputs even if that ever changes.
        g.node_inputs(joining_region)
            .iter()
            .filter(|&v| g.value_kind(v).is_control())
            .map(|ctrl_val| {
                let producer = g.producer(ctrl_val);
                // Case 1: arm arrives via an If's true edge.
                // The If's outputs are [true_ctrl, false_ctrl]; true_ctrl is
                // output index 0.  We detect this by checking whether
                // `ctrl_val` is the first output of the If node.
                if matches!(g.node_kind(producer), NodeKind::If) {
                    let if_outputs = g.node_outputs(producer);
                    if !if_outputs.is_empty() && if_outputs[0] == ctrl_val {
                        // True edge of an If → query in joining_region.
                        return joining_region;
                    }
                }
                // Case 2: unconditional branch, false edge, or other.
                self.ctrl_source_region(ctrl_val)
            })
            .collect()
    }

    /// Given a control value, walk back to find the `Region` or `Entry`
    /// node it ultimately comes from.  This handles the case where the
    /// control passes through an `If` output (the `If`'s own input is a
    /// Region control output).
    fn ctrl_source_region(&self, ctrl_val: ValueId) -> NodeId {
        let g = self.function.graph();
        let mut curr = g.producer(ctrl_val);
        // Walk through branching control-consumer nodes back to the nearest Region.
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
    /// We iterate the guards on `value` and intersect those whose `guard_node`
    /// dominates `region`.  This makes each query O(guards_on_v × domtree-depth)
    /// rather than requiring eager enumeration of all dominated regions at
    /// build time.
    fn resolve_leaf(&self, value: ValueId, region: NodeId) -> Interval {
        let ty = self.function.value_type_opt(value);
        let type_mask = ty.map_or(u128::MAX, |t| t.bit_mask_u128());

        // Collect the intersection of all guards that dominate `region`.
        let guard = self.dominating_guard(value, region);

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

/// If `value`'s producer is `Add(X, IntConst(c))`, return `(X, interval')`
/// where `interval'` is `interval` shifted by `-c` (mod the operand width) —
/// the bound that `X` inherits from a guard on `X + c`.  Returns `None` when
/// the value is not such an add, or when the shift wraps (the shifted interval
/// would straddle zero and can't be represented as a single `[lo, hi]`).
fn add_operand_shifted_interval(
    function: &strider_ir::Function,
    value: ValueId,
    interval: Interval,
) -> Option<(ValueId, Interval)> {
    let node = function.producer(value);
    if !matches!(
        function.node_kind(node),
        NodeKind::IntBinaryOp(IntBinaryOp::Add)
    ) {
        return None;
    }
    let [a, b] = function.graph().node_inputs_exact::<2>(node).ok()?;
    // Exactly one operand must be the constant addend.
    let (operand, c) = match (function.int_const_u128(a), function.int_const_u128(b)) {
        (None, Some(c)) => (a, c),
        (Some(c), None) => (b, c),
        _ => return None,
    };
    let mask = function.value_type_opt(operand)?.bit_mask_u128();
    // `X = value - c` (mod width): shift both interval ends by `-c`.
    let lo = interval.lo.wrapping_sub(c) & mask;
    let hi = interval.hi.wrapping_sub(c) & mask;
    // Only a non-wrapping result is a representable `[lo, hi]` interval.
    (lo <= hi).then_some((operand, Interval { lo, hi }))
}

/// Returns `true` when KnownBits proves the sign bit of `value` is
/// zero (i.e. the value is always non-negative in signed interpretation).
fn is_sign_bit_known_zero(
    function: &strider_ir::Function,
    value: ValueId,
    known: &KnownBitsMap,
) -> bool {
    let Some(ty) = function.value_type_opt(value) else {
        return false;
    };
    let Some(type_mask) = crate::opt::known_bits::u64_type_mask(ty) else {
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
        let Some(ty) = function.value_type_opt(value_id) else {
            continue;
        };
        let Some(type_mask_u64) = crate::opt::known_bits::u64_type_mask(ty) else {
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
        // If's outputs: [true_ctrl, false_ctrl].  We model BOTH edges:
        // the condition implies an interval on the true edge and its negation
        // implies an interval on the false edge.
        let if_outputs = function.node_outputs(if_node);
        if if_outputs.len() < 2 {
            continue;
        }
        let true_ctrl = if_outputs[0];
        let false_ctrl = if_outputs[1];

        // The condition value is If's input[1].
        let if_inputs: Vec<ValueId> = function.node_inputs(if_node).iter().collect();
        if if_inputs.len() < 2 {
            continue;
        }
        let cond_value = if_inputs[1];

        // Per edge: shape-match the condition (under the edge's polarity) to
        // extract the guarded `(value, interval)`, then key the guard by the
        // unique control consumer of that edge — the node whose dominance the
        // range query checks against.  This survives `RegionCollapse` deleting
        // the dispatch Region: the guard then keys on whatever control node
        // (IndirectBranch / If / …) directly consumes the edge.
        for (edge_ctrl, edge_taken) in [(true_ctrl, true), (false_ctrl, false)] {
            // Shape-match the condition under this edge's polarity.
            let Some((guarded_value, guard_interval)) =
                guard_from_compare(function, cond_value, edge_taken, known)
            else {
                continue;
            };

            // Soundness gate: only attach the guard to an edge whose consumer
            // is reached EXCLUSIVELY via that edge.  In the converged IR this
            // analysis runs on, `RegionCollapse` has dissolved every
            // single-predecessor `Region`, so a `Region` that still consumes an
            // edge is a genuine control merge — the guard does not hold for the
            // other predecessors, so skip it.  Every other control consumer
            // (If/Call/IndirectBranch/Return/…) has exactly one control input by
            // signature, so the guard is exclusive there.
            let Some(consumer) = single_control_consumer(function, edge_ctrl) else {
                continue;
            };
            if matches!(function.node_kind(consumer), NodeKind::Region) {
                continue;
            }

            // Record lazily: push the (consumer, interval) pair.  Dominance is
            // checked at query time in resolve_leaf.  (No trivial-phi chase is
            // needed: `PhiCollapse` has already eliminated single-input phis, so
            // the guarded value is its own underlying base.)
            guards
                .entry(guarded_value)
                .or_default()
                .push((consumer, guard_interval));

            // Back-propagate the bound through `Add(X, const)`: a guard on
            // `X + c` bounds `X = (guarded) - c` too (shift the interval by
            // `-c`, recorded only while it stays non-wrapping).  This is the
            // masked / offset switch shape — the guard sits on `idx - K` while
            // the dispatch indexes `idx`, so `idx` inherits the shifted bound.
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

/// Shape-matches an `If` condition under a given edge polarity and returns
/// the `(guarded_value, interval)` pair it constrains on that edge, or `None`
/// when the shape yields no USEFUL upper-bounded interval.
///
/// `edge_taken` selects the edge: `true` for the If's true successor (the
/// condition holds), `false` for the false successor (its negation holds).
///
/// Only upper-bounded intervals matter for table sizing.  A purely
/// lower-bounded constraint (e.g. `v >= N` from the false edge of `v < N`) is
/// useless, so we return `None` (top) rather than recording it.
///
/// The lowered `<=` shape `Xor(Less(N, v), 1):I1` is NOT handled here: by the
/// time `compute_value_ranges` runs, `IfCondInversion` has already rewritten
/// `If(Xor(C,1))` to `If(C)` with swapped branches, so the condition is already
/// a bare `Less`/`Sless`.
///
/// Recognised compare shapes (`Less`/`Sless`; `Sless` gated on
/// KnownBits sign-bit = 0):
///
/// - `Less(v, IntConst(N))` — `v < N`.
///   true edge → `v ∈ [0, N-1]` (N>0); false edge → `v >= N` (lower-only → None).
/// - `Less(IntConst(N), v)` — `N < v`.
///   true edge → `v >= N+1` (lower-only → None); false edge → `v <= N` → `[0, N]`.
///
/// Shape-matches a bare `Less`/`Sless` comparison value under `edge_taken`
/// polarity, returning the upper-bounded interval it implies on that edge
/// (or `None` for an unrecognised shape / a lower-only constraint).
fn guard_from_compare(
    function: &strider_ir::Function,
    cmp_value: ValueId,
    edge_taken: bool,
    known: &KnownBitsMap,
) -> Option<(ValueId, Interval)> {
    let g = function.graph();
    let producer = g.producer(cmp_value);
    let NodeKind::IntCmpOp(op @ (IntCmpOp::Less | IntCmpOp::Sless)) = *g.node_kind(producer) else {
        return None;
    };
    let [lhs, rhs] = g.node_inputs_exact::<2>(producer).ok()?;

    // Identify which operand is the constant and which is the guarded value.
    // `Less(v, IntConst(N))`  → const on RHS, `v < N`.
    // `Less(IntConst(N), v)`  → const on LHS, `N < v`.
    let (guarded, n, const_on_rhs) =
        match (function.int_const_u128(lhs), function.int_const_u128(rhs)) {
            // Both const (or neither): nothing to bound — const-fold handles the
            // both-const case; skip.
            (Some(_), Some(_)) | (None, None) => return None,
            (None, Some(n)) => (lhs, n, true),
            (Some(n), None) => (rhs, n, false),
        };

    // The guarded operand must not itself be a constant (caught above) and
    // must be an integer wider than a bool.
    let ty = function.value_type_opt(guarded)?;
    if !ty.is_integer() || ty == ValueType::I1 {
        return None;
    }
    if op == IntCmpOp::Sless && !is_sign_bit_known_zero(function, guarded, known) {
        return None;
    }
    let type_mask = ty.bit_mask_u128();

    // Determine whether `guarded` ends up UPPER-bounded on this edge.
    //
    //   const_on_rhs  edge_taken  meaning           bound
    //   true (v<N)    true        v < N             [0, N-1]  (N>0)
    //   true (v<N)    false       v >= N            lower-only → None
    //   false (N<v)   true        N < v ⟹ v >= N+1  lower-only → None
    //   false (N<v)   false       !(N<v) ⟹ v <= N   [0, N]
    match (const_on_rhs, edge_taken) {
        (true, true) => {
            // v < N → [0, N-1].  N == 0 is an impossible unsigned guard.
            if n == 0 {
                return None;
            }
            Some((
                guarded,
                Interval {
                    lo: 0,
                    hi: n.saturating_sub(1).min(type_mask),
                },
            ))
        }
        (false, false) => {
            // v <= N → [0, N].
            Some((
                guarded,
                Interval {
                    lo: 0,
                    hi: n.min(type_mask),
                },
            ))
        }
        // Lower-only constraints carry no useful upper bound.
        (true, false) | (false, true) => None,
    }
}

/// Returns the unique control node that consumes `ctrl_val`.
///
/// A `Control`-typed value output is consumed by exactly one control node
/// (each control edge has a single sink), so the first consumer is the only
/// one.  `None` when nothing consumes it (a dead edge).
fn single_control_consumer(function: &strider_ir::Function, ctrl_val: ValueId) -> Option<NodeId> {
    function
        .graph()
        .value_uses(ctrl_val)
        .map(|(consumer, _slot)| consumer)
        .next()
}
