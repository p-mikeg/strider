//! Jump-table arm for the tier-2 indirect-branch classifier.
//!
//! Round R4 extension.  Recognises the canonical jump-table dispatch
//! shape — `Load(IntAdd(IntConst(base), IntMul(idx, IntConst(stride))))`
//! and its commutative variants — proves an upper bound `N` on the
//! index `idx`, reads the `N` table entries from a caller-supplied
//! [`ReadOnlyMemory`], and returns
//! [`ResolvedTargets::Multiple([table[0], …, table[N-1]])`].
//!
//! ## Soundness
//!
//! Two independent gates must hold for the classifier to commit to
//! `Multiple`:
//!
//! 1. **Bounded index.**  Either [`KnownBits`] proves `idx`'s upper
//!    bits are zero (so `idx <= mask` and the bound is `mask + 1`),
//!    or the control-flow path that reaches this branch must traverse
//!    an `If(IntCmpOp::{Less|LessEqual|Sless|SlessEqual}(idx, IntConst(N)))`
//!    edge whose true side dominates the dispatch.  Both bounds are
//!    *upper* bounds on `idx`'s runtime value; we use them
//!    monotonically.  Mixed-bound joins fail closed.
//!
//! 2. **Complete table read.**  *Every* entry from `table[0]` through
//!    `table[N-1]` must read back through the rom.  A partial read
//!    (e.g. `table` straddles the rodata/text boundary or runs past
//!    end-of-section) returns `None` — we never produce a `Multiple`
//!    that excludes some valid runtime targets, because the
//!    orchestrator would then wire a CFG with missing edges and
//!    silently corrupt the analysis.
//!
//! Failing either gate means the classifier returns `None`; the
//! orchestrator defers the branch to a later iteration or surfaces
//! `UnresolvedIndirectBranch` at fixed point.  No panic, no partial
//! commitment, no over-approximation.
//!
//! Over-approximating bounds (proving `idx <= N` when the runtime
//! value never exceeds `M < N`) is sound — extra targets in the
//! `Multiple` add unreachable CFG edges that downstream analysis
//! treats as dead code.  Under-approximating is *not* sound: missing
//! a runtime target means the CFG omits a real edge.

#![allow(clippy::module_name_repetitions)]

use std::collections::HashSet;

use cfg::test_api::ResolvedTargets;
use ir::node::{NodeId, NodeKind, NodeOutputId, NodeOutputType};
use ir::{BuiltFunctionGraph, IntBinaryOp, IntCmpOp};
use opt::ReadOnlyMemory;
use rsleigh::VnSpace;

/// Maximum number of jump-table entries we willingly enumerate.
///
/// A `mask`-derived bound from [`KnownBits`] can be as large as
/// `u32::MAX + 1` if the mask is all-ones; without this cap a buggy
/// known-bits result would force us to iterate through 4 GiB of
/// entries.  Real jump tables emitted by gcc / clang are bounded by
/// the source-level `switch` arm count, almost always well under
/// 4096.  Tables larger than this cap are unusual enough that we
/// prefer `None` (defer to `UnresolvedIndirectBranch`) over the
/// pathological enumeration cost.
const MAX_TABLE_ENTRIES: u64 = 4096;

/// Top-level classifier hook for the jump-table arm.  Called by
/// [`super::classify::classify_anchor`] when the anchor's producer is
/// a [`NodeKind::Load`].
///
/// `anchor_output` is the placeholder Return's value-input slot.
/// `rom` is the read-only memory image — almost always the ELF's
/// `.rodata` + `.text` view for callers that load real binaries.
/// `link_register_vn` is unused here (jump tables don't dispatch to
/// the link register), but kept symmetric with the other arms in
/// case a future refactor needs it.
#[must_use]
pub fn classify_jump_table(
    graph: &BuiltFunctionGraph,
    anchor_output: NodeOutputId,
    rom: Option<&dyn ReadOnlyMemory>,
    _link_register_vn: Option<rsleigh::Vn>,
) -> Option<ResolvedTargets> {
    // Step 1: structural shape match.  `match_jump_table_shape`
    // returns the `idx` value and the `(base, stride, space)` triple
    // — everything we need to enumerate entries.  Falls through to
    // None for every shape that isn't an honest jump-table dispatch.
    let shape = match_jump_table_shape(graph, anchor_output)?;

    // Step 2: bound the index.  Two strategies, tried in order:
    //   (a) KnownBits — purely structural inspection of the IR;
    //       cheap; works whenever the shape contains an explicit
    //       AND-mask (`idx & 0x7` etc.).
    //   (b) Predecessor-If walk — looks for an `If(idx < N)` on the
    //       control path leading to the dispatch's region.  Slower
    //       but covers the gcc-emitted "compare-and-branch then
    //       indirect" pattern that has no AND-mask.
    let bound = bound_via_known_bits(graph, shape.idx_output)
        .or_else(|| bound_via_predecessor_if(graph, anchor_output, shape.idx_output))?;

    // Step 3: enforce the per-call enumeration cap.  Returning None
    // here is sound: the orchestrator will defer; if a future
    // iteration tightens the bound (e.g. RedundantPhis exposes a
    // narrower KnownBits result) the table will resolve.
    if bound == 0 || bound > MAX_TABLE_ENTRIES {
        return None;
    }

    // Step 4: read the table.  Failing closed (None on partial read)
    // is the soundness guard: a partial Multiple would omit valid
    // runtime targets and the orchestrator would wire a CFG missing
    // those edges.  See `read_table_entries` for the full rule.
    let rom = rom?;
    let targets = read_table_entries(rom, shape.base, shape.stride, bound, shape.entry_size)?;

    // Sort + dedup so the resulting Multiple is canonical (matches
    // the orchestrator's edge-set comparison protocol — see the
    // ValuePhi arm in classify.rs for the same rationale).
    let mut targets = targets;
    targets.sort_unstable();
    targets.dedup();
    Some(ResolvedTargets::Multiple(targets))
}

// ── Shape match ──────────────────────────────────────────────────────────────

/// The structural fields extracted from a jump-table-shaped Load.
#[derive(Debug, Clone, Copy)]
struct JumpTableShape {
    /// The `IntConst` table base — the address of `table[0]`.
    base: u64,
    /// The `IntConst` per-entry stride in bytes.  Almost always 4
    /// (32-bit pointer table) or 8 (64-bit pointer table); we
    /// preserve whatever the IR says so unusual ABIs (offset tables,
    /// 16-bit relative offsets) work in principle.
    stride: u64,
    /// The value output for the index (`idx`) — what bound resolution
    /// must constrain.
    idx_output: NodeOutputId,
    /// The Load's per-entry size in bytes (matches the Load's output
    /// type).  Distinct from `stride` because some tables have
    /// padding between entries (`stride > entry_size`); we read
    /// `entry_size` bytes at each `base + i * stride`.
    entry_size: usize,
    /// The Load's address space (almost always `VnSpace::RAM` /
    /// `default`; preserved verbatim should a future round need to
    /// pass it through to the rom read instead of the current hard-
    /// coded `VnSpace::RAM`).
    #[allow(dead_code)]
    space: VnSpace,
}

/// Recognises the canonical jump-table address shape on the producer
/// of `anchor_output`.
///
/// Accepted shapes (commutativity of `+` and `*` is honoured — gcc
/// and clang emit either operand order depending on optimisation
/// level + register allocator decisions):
///
///   * `Load[ IntAdd( IntConst(base), IntMul(idx,        IntConst(stride)) ) ]`
///   * `Load[ IntAdd( IntConst(base), IntMul(IntConst(stride), idx       ) ) ]`
///   * `Load[ IntAdd( IntMul(idx,        IntConst(stride)), IntConst(base) ) ]`
///   * `Load[ IntAdd( IntMul(IntConst(stride), idx       ), IntConst(base) ) ]`
///
/// Every other shape — including degenerate `Load[IntConst(addr)]`
/// (a simple global read; the `IntConst` arm in `classify.rs` would
/// handle that one if it were a dispatch target) — returns None and
/// defers to whatever later arms exist.
fn match_jump_table_shape(
    graph: &BuiltFunctionGraph,
    anchor_output: NodeOutputId,
) -> Option<JumpTableShape> {
    // The producer must be a Load.  classify.rs already routes here
    // only on Load, but we re-check defensively so this function is
    // testable in isolation.
    let load_node = graph.graph.get_node_from_output(anchor_output);
    let NodeKind::Load(space) = *graph.graph.node_kind(load_node) else {
        return None;
    };
    // Load output's type tells us the per-entry byte size.  Reject
    // float/bool typed loads — jump tables hold integer pointers.
    let ty = graph.graph.output_kind(anchor_output).as_value()?;
    if !ty.is_integer() {
        return None;
    }
    let entry_size = ty.byte_size();

    // Load inputs: [memory_token, addr].
    let load_inputs: Vec<NodeOutputId> = graph.graph.node_inputs(load_node).into_iter().collect();
    let addr_output = *load_inputs.get(1)?;

    // The address must be IntAdd(...).
    let add_node = graph.graph.get_node_from_output(addr_output);
    if !matches!(
        graph.graph.node_kind(add_node),
        NodeKind::IntBinaryOp(IntBinaryOp::Add)
    ) {
        return None;
    }
    let [add_lhs, add_rhs] = graph.graph.node_inputs_exact::<2>(add_node).ok()?;

    // Try both operand orderings: (const, mul) and (mul, const).
    // `extract_base_and_mul` returns Some when one operand is a
    // const-typed IntConst and the other is an IntMul we can crack
    // open.
    extract_base_and_mul(graph, add_lhs, add_rhs, entry_size, space)
        .or_else(|| extract_base_and_mul(graph, add_rhs, add_lhs, entry_size, space))
}

/// Helper for [`match_jump_table_shape`]: tries to interpret
/// `(base_candidate, mul_candidate)` as `(IntConst(base),
/// IntMul(idx, IntConst(stride)))` — accepting both orderings inside
/// the multiplication too.  Returns None on any structural mismatch.
fn extract_base_and_mul(
    graph: &BuiltFunctionGraph,
    base_candidate: NodeOutputId,
    mul_candidate: NodeOutputId,
    entry_size: usize,
    space: VnSpace,
) -> Option<JumpTableShape> {
    // The base side must be an IntConst — otherwise we can't pin
    // `table[0]`'s address and rom-reading is impossible.
    let base = graph.int_const_val(base_candidate)?;

    // The mul side must be an IntMul of (idx, IntConst(stride)) in
    // either order.
    let mul_node = graph.graph.get_node_from_output(mul_candidate);
    if !matches!(
        graph.graph.node_kind(mul_node),
        NodeKind::IntBinaryOp(IntBinaryOp::Mul)
    ) {
        return None;
    }
    let [mul_lhs, mul_rhs] = graph.graph.node_inputs_exact::<2>(mul_node).ok()?;

    // (idx, IntConst(stride))
    if let Some(stride) = graph.int_const_val(mul_rhs) {
        return Some(JumpTableShape {
            base,
            stride,
            idx_output: mul_lhs,
            entry_size,
            space,
        });
    }
    // (IntConst(stride), idx)
    if let Some(stride) = graph.int_const_val(mul_lhs) {
        return Some(JumpTableShape {
            base,
            stride,
            idx_output: mul_rhs,
            entry_size,
            space,
        });
    }
    None
}

// ── Bound via KnownBits ──────────────────────────────────────────────────────

/// Walks `idx_output`'s producer-shape using a *local* known-bits
/// computation to compute an upper bound on `idx`'s runtime value.
///
/// Semantics: a known-bits mask of `M` (the OR of all bits that
/// could be 1) means `idx <= M`, so the count of distinct values is
/// at most `M + 1`.  Returns `Some(M + 1)` when known-bits proves
/// any non-trivial upper bound (i.e. some bits are known zero), and
/// None otherwise.
///
/// We don't reuse the [`opt::KnownBits`] pass directly because that
/// pass mutates the graph (replacing fully-determined outputs with
/// IntConst); our caller is non-mutating.  The local recurrence
/// here is the same propagation rules pared down to the cases that
/// actually narrow an index value: `IntConst`, `And` with a const,
/// `Truncate`, `Extend(ZeroExtend)`, and `ShiftRight` by a const.
#[must_use]
pub fn bound_via_known_bits(
    graph: &BuiltFunctionGraph,
    idx_output: NodeOutputId,
) -> Option<u64> {
    // Output type: only integer-typed indices make sense as table
    // indices.  Reject everything else (Bool, F32, F64, …).
    let ty = graph.graph.output_kind(idx_output).as_value()?;
    if !ty.is_integer() {
        return None;
    }
    // Type mask sets the maximum possible value (e.g. 0xff for U8).
    // KnownBits at most narrows below this; if no narrowing is
    // possible we return None so the predecessor-If fallback gets a
    // chance.
    let type_mask = ty.get_unsigned_int(u64::MAX)?;

    let mask = compute_max_mask(graph, idx_output, type_mask, &mut HashSet::new())?;
    // mask + 1 is the count of distinct values in [0, mask].
    // Saturating to u64::MAX covers the pathological mask == u64::MAX
    // case (no narrowing); in that case we conservatively report
    // None so the caller falls back rather than try to enumerate
    // 2^64 entries.
    if mask == type_mask {
        return None;
    }
    mask.checked_add(1)
}

/// Computes a conservative upper-bound mask on the value of `out`'s
/// runtime contents — i.e. a `M` such that `value <= M` always.
/// Returns the input type's full mask when no narrowing applies.
///
/// Recursive on producer kind.  Visited-set protects against IR
/// cycles (Phi-of-itself shapes the validator otherwise allows
/// across loop back-edges).
fn compute_max_mask(
    graph: &BuiltFunctionGraph,
    out: NodeOutputId,
    type_mask: u64,
    visited: &mut HashSet<NodeOutputId>,
) -> Option<u64> {
    if !visited.insert(out) {
        // Cycle: return the most permissive bound (no narrowing).
        // Sound: an over-approximation here can only widen the
        // final mask, not under-approximate it.
        return Some(type_mask);
    }
    let node = graph.graph.get_node_from_output(out);
    match *graph.graph.node_kind(node) {
        // Constant: the mask is exactly the constant's value.
        NodeKind::IntConst(k) => {
            #[allow(clippy::cast_possible_truncation)]
            let k64 = k as u64;
            Some(k64 & type_mask)
        }
        // AND with a const masks the value below the const.
        // CORRECTNESS: max(a & m) <= m for any non-negative a, regardless
        // of `a`'s own bound — that's the whole point of using AND as a
        // mask.  We take the min of the two operand bounds so e.g.
        // `(x & 0xff) & 0x7` correctly bounds at 0x7.
        NodeKind::IntBinaryOp(IntBinaryOp::And) => {
            let inputs = graph.graph.node_inputs_exact::<2>(node).ok()?;
            let l = compute_max_mask(graph, inputs[0], type_mask, visited).unwrap_or(type_mask);
            let r = compute_max_mask(graph, inputs[1], type_mask, visited).unwrap_or(type_mask);
            Some(l.min(r))
        }
        // Truncate: upper bits are dropped, narrowing to the output type.
        NodeKind::Truncate => {
            // The truncate's output mask is already type_mask; nothing
            // beyond what type_mask captured.  Returning type_mask is
            // the conservative answer.
            Some(type_mask)
        }
        // ZeroExtend: upper bits are explicitly zero; the bound on the
        // wider value is the bound on the narrower value.  We don't
        // have access to the input's narrower mask here, but the
        // wider-type mask still narrows to it because we mask with
        // type_mask at every step.
        NodeKind::Extend(ir::ExtendOp::ZeroExtend) => {
            let inputs = graph.graph.node_inputs_exact::<1>(node).ok()?;
            let inner_kind = graph.graph.output_kind(inputs[0]).as_value()?;
            if !inner_kind.is_integer() {
                return None;
            }
            let inner_mask = inner_kind.get_unsigned_int(u64::MAX)?;
            // The extend zeros bits above inner_mask, so the result is
            // bounded by inner_mask intersected with our outer
            // type_mask.
            let inner_bound =
                compute_max_mask(graph, inputs[0], inner_mask, visited).unwrap_or(inner_mask);
            Some(inner_bound & type_mask)
        }
        // Logical right-shift by a const: the upper `shift` bits become
        // zero.  CORRECTNESS: `(a >> s) <= type_mask >> s` for any
        // non-negative `a`, so the post-shift mask is `type_mask >> s`.
        NodeKind::IntBinaryOp(IntBinaryOp::ShiftRight) => {
            let inputs = graph.graph.node_inputs_exact::<2>(node).ok()?;
            let shift = graph.int_const_val(inputs[1])?;
            if shift >= 64 {
                // Pathological — shift out everything; result is 0.
                return Some(0);
            }
            // Combine with the lhs's own bound for tighter results
            // when the lhs already has known zeros in the upper bits.
            let lhs_bound =
                compute_max_mask(graph, inputs[0], type_mask, visited).unwrap_or(type_mask);
            Some((lhs_bound >> shift) & (type_mask >> shift))
        }
        // Anything else: don't narrow.
        _ => Some(type_mask),
    }
}

// ── Bound via predecessor-If walk ────────────────────────────────────────────

/// Walks the control-flow chain *backwards* from the placeholder
/// Return at `anchor_output`'s consumer until it finds an
/// `If(IntCmp(idx_output, IntConst(N)))` whose dominating edge is
/// the true branch.  Returns `Some(N)` when the bound is proved, or
/// None when:
///
///   * No `If` on the path tests `idx_output`.
///   * The walk reaches the function entry (no more predecessors).
///   * A multi-predecessor `ControlState` (a join point) is reached
///     where any incoming path doesn't have the bound.  Joining
///     mixed-bound paths fails closed: the runtime path could be
///     either, so we can't soundly assume the bound holds.
///   * A cycle is detected (back-edge of a loop).  Loops can have
///     `idx` mutated mid-iteration; our walk isn't strong enough
///     to reason about that.
///
/// CORRECTNESS: the bound from this walk is an upper bound on
/// `idx_output`'s value at the placeholder Return: every runtime
/// execution that reaches the dispatch must have traversed at
/// least one of the matched `If` edges, and on that edge the
/// `IntCmp` must have evaluated true (otherwise the false branch
/// would have been taken and we'd never reach the dispatch).
/// `IntCmp(idx, N)` evaluating true under {Less, LessEqual, Sless,
/// SlessEqual} bounds `idx` above by `N` or `N+1`.
#[must_use]
pub fn bound_via_predecessor_if(
    graph: &BuiltFunctionGraph,
    anchor_output: NodeOutputId,
    idx_output: NodeOutputId,
) -> Option<u64> {
    // Find the placeholder Return that consumes the anchor.  This
    // is the start of our backward walk.
    //
    // The placeholder Return's input slot 0 is its Control input; we
    // walk upward through Controls looking for an If whose true
    // branch leads to this Return.
    let return_node = find_anchor_consumer_return(graph, anchor_output)?;
    // Slot 0 = control; see node_signature::expected_signature for
    // Return: `inputs: [CTRL, MEM]; in_tail: RET`.
    let control_in = *graph.graph.node_inputs(return_node).get(0)?;

    let mut visited: HashSet<NodeId> = HashSet::new();
    walk_control_for_if_bound(graph, control_in, idx_output, &mut visited)
}

/// Locates the (single) Return node that consumes `anchor_output` —
/// that's the placeholder `Return(target_value)` the strider lift
/// emits for `UnresolvedIndirectBranch` regions.  Returns None when
/// no consumer is a Return — the producer-shape match should have
/// gated us out before reaching this point, so this is defensive.
fn find_anchor_consumer_return(
    graph: &BuiltFunctionGraph,
    anchor_output: NodeOutputId,
) -> Option<NodeId> {
    for (consumer_id, _) in graph.graph.output_uses(anchor_output) {
        if matches!(graph.graph.node_kind(consumer_id), NodeKind::Return) {
            return Some(consumer_id);
        }
    }
    None
}

/// The recursive heart of the predecessor-If walk.  `control_out` is
/// the Control output we're currently looking at — i.e. the
/// `Control` input of whoever's downstream.  Returns the proved
/// bound (or None) for the path on the way to here.
fn walk_control_for_if_bound(
    graph: &BuiltFunctionGraph,
    control_out: NodeOutputId,
    idx_output: NodeOutputId,
    visited: &mut HashSet<NodeId>,
) -> Option<u64> {
    let producer = graph.graph.get_node_from_output(control_out);
    if !visited.insert(producer) {
        // Cycle (loop back-edge).  Loops can rewrite `idx` per
        // iteration; without loop-level reasoning the bound from
        // the loop entry doesn't hold inside the body.  Fail closed.
        return None;
    }

    match graph.graph.node_kind(producer) {
        // If's outputs: [true_control, false_control].
        // We're on the path to the dispatch through this If — figure
        // out which branch led to us, then check whether that branch
        // bounds idx.
        NodeKind::If => {
            // Which output of the If is `control_out`?  The output
            // index distinguishes true (0) from false (1) per the
            // node_signature for `If`: `outputs: [CTRL, CTRL]`.
            let (_, output_idx) = graph.graph.output_definition(control_out);
            let if_inputs = graph.graph.node_inputs_exact::<2>(producer).ok()?;
            // If input slot 0 = ctrl predecessor, slot 1 = condition.
            let cond_out = if_inputs[1];
            let on_true = output_idx == 0;
            // Try this If's condition; if it doesn't bound idx, walk
            // up through the If's own control predecessor.  Either
            // failure mode is sound: an If we can't crack open
            // becomes a transparent control-flow node we walk
            // through.
            if let Some(b) = bound_from_if_condition(graph, cond_out, idx_output, on_true) {
                return Some(b);
            }
            walk_control_for_if_bound(graph, if_inputs[0], idx_output, visited)
        }
        // ControlState merges multiple predecessors — every
        // predecessor's path must independently prove the bound, and
        // we take the *max* (the join's effective bound is the
        // weakest of any incoming path).  If any predecessor returns
        // None, the join's bound is None (mixed-bound join → fail
        // closed).
        NodeKind::ControlState => {
            let inputs: Vec<NodeOutputId> =
                graph.graph.node_inputs(producer).into_iter().collect();
            if inputs.is_empty() {
                return None;
            }
            let mut combined: u64 = 0;
            for &pred in &inputs {
                // Clone visited so cycles in one predecessor's
                // sub-walk don't poison the others.  Without this,
                // a back-edge from one predecessor would mark the
                // whole join unreachable for every later
                // predecessor.
                let mut local = visited.clone();
                let bound = walk_control_for_if_bound(graph, pred, idx_output, &mut local)?;
                combined = combined.max(bound);
            }
            Some(combined)
        }
        // Entry: no more predecessors; we've walked the whole
        // function without finding a bound.
        NodeKind::Entry => None,
        // Other control-producing kinds (the `Call`'s control output
        // for a function that returns into our region, etc.) are
        // walked through transparently — they don't bound `idx`.
        // We follow the node's slot-0 input as the control
        // predecessor when the node has one; otherwise return None.
        _ => {
            let inputs = graph.graph.node_inputs(producer);
            let first = inputs.get(0).copied()?;
            // Only walk through if the input is a Control output —
            // otherwise we'd derail into data flow.
            if !graph.graph.output_kind(first).is_control() {
                return None;
            }
            walk_control_for_if_bound(graph, first, idx_output, visited)
        }
    }
}

/// Inspects an `If`'s boolean condition for an `IntCmp(idx, IntConst(N))`
/// shape where `idx` matches our target index value.  Returns the
/// bound when the condition implies `idx <= some_N`.
///
/// `on_true_branch` indicates whether we reached the dispatch via
/// the If's true output (true) or false output (false).  Comparison
/// ops bound `idx` differently depending on which branch dominates:
///
///   * `idx < N`   true  → `idx <= N - 1` → bound is `N` (count of values).
///   * `idx <= N`  true  → `idx <= N`     → bound is `N + 1`.
///   * `idx < N`   false → `idx >= N`     → no upper bound.
///   * `idx <= N`  false → `idx > N`      → no upper bound.
///
/// We only return Some on the true-side variants; the false side
/// gives a *lower* bound which doesn't help here.  An over-cautious
/// None is sound; the orchestrator may try again with a stronger
/// classifier next iteration.
fn bound_from_if_condition(
    graph: &BuiltFunctionGraph,
    cond_out: NodeOutputId,
    idx_output: NodeOutputId,
    on_true_branch: bool,
) -> Option<u64> {
    if !on_true_branch {
        return None;
    }
    let cmp_node = graph.graph.get_node_from_output(cond_out);
    let NodeKind::IntCmpOp(op) = *graph.graph.node_kind(cmp_node) else {
        return None;
    };
    let [lhs, rhs] = graph.graph.node_inputs_exact::<2>(cmp_node).ok()?;
    // Check both orderings: `idx < N` and `N > idx`-shaped (the IR
    // doesn't have GreaterThan directly, but the orchestrator may
    // see swapped operands when ConstantFold normalises a `swap`
    // away).
    let (idx_side, const_side, swapped) = if same_value(graph, lhs, idx_output) {
        (lhs, rhs, false)
    } else if same_value(graph, rhs, idx_output) {
        (rhs, lhs, true)
    } else {
        return None;
    };
    let _ = idx_side; // kept for symmetry / readability
    let n = graph.int_const_val(const_side)?;

    // CORRECTNESS for the `swapped` case: in `IntCmp::Less(N, idx)`
    // taken-true we have `N < idx`, which is a *lower* bound on
    // idx — no upper bound.  We therefore return None on swapped
    // for asymmetric ops.
    match op {
        // idx < N (true) → bound = N.
        IntCmpOp::Less | IntCmpOp::Sless if !swapped => Some(n),
        // idx <= N (true) → bound = N + 1.
        IntCmpOp::LessEqual | IntCmpOp::SlessEqual if !swapped => n.checked_add(1),
        // Equality is symmetric: `idx == N` taken-true means `idx == N`
        // exactly — bound is `N + 1` (idx is one of {0, …, N}, but we
        // overapproximate to N+1 as the count of distinct values up
        // through and including N; a tighter Single(table[N]) is
        // possible but not produced here to keep arms uniform).
        IntCmpOp::Equal => n.checked_add(1),
        _ => None,
    }
}

/// Defines value identity for the predecessor-If walk.
///
/// Two `NodeOutputId`s match when:
///   * They refer to the same output (the trivial case).
///   * One is the OUTPUT of a single-input `ControlPhi` / `ValuePhi`
///     whose only value input is the other.  This covers the common
///     pattern where the entry region's `If(idx < N)` reads idx
///     directly while the dispatch region's `Load[..idx*stride..]`
///     reads idx through the dispatch region's entry phi.  Without
///     RedundantPhis (which intermediate orchestrator iterations
///     omit) those two reads have different `NodeOutputId`s even
///     though they're trivially identical values.
///
/// We follow the chain transitively so deeper phi nests collapse
/// the same way.  A visited set protects against cycles (back-edges
/// of unsimplified loops); on cycle, we return false rather than
/// looping — same conservative direction as `walk_control_for_if_bound`.
fn same_value(graph: &BuiltFunctionGraph, a: NodeOutputId, b: NodeOutputId) -> bool {
    // Bidirectionally chase trivial phis: see if either side reduces
    // to the other.  Cap depth to avoid pathological chains.
    fn root(graph: &BuiltFunctionGraph, mut out: NodeOutputId) -> NodeOutputId {
        let mut budget = 64usize;
        let mut visited: HashSet<NodeOutputId> = HashSet::new();
        while budget > 0 && visited.insert(out) {
            let node = graph.graph.get_node_from_output(out);
            match graph.graph.node_kind(node) {
                NodeKind::ControlPhi(_) | NodeKind::ValuePhi => {
                    let inputs: Vec<NodeOutputId> =
                        graph.graph.node_inputs(node).into_iter().collect();
                    // Slot 0 is the phi-token; slots 1.. are values.
                    // A trivial phi has exactly one value input.
                    if inputs.len() == 2 {
                        out = inputs[1];
                        budget -= 1;
                        continue;
                    }
                    return out;
                }
                _ => return out,
            }
        }
        out
    }
    root(graph, a) == root(graph, b)
}

// ── Read table entries ───────────────────────────────────────────────────────

/// Reads `count` table entries of `entry_size` bytes each from
/// `rom`, stride `stride`, starting at virtual address `base`.
/// Returns the read targets in index order, or None if any read
/// fails.
///
/// CORRECTNESS: failing closed on any partial read is the soundness
/// guard.  A jump table's runtime semantics are: at index i, the
/// program loads `*(base + i*stride)` and dispatches there.  If the
/// rom can't satisfy any entry's read, we don't know that entry's
/// runtime target — but the program does, so a Multiple that omits
/// it would induce a CFG missing real edges.  The orchestrator
/// would then mistakenly believe the function is fully analysed.
/// Returning None forces the orchestrator to fall back to
/// `UnresolvedIndirectBranch`, which is correct: we genuinely don't
/// know the targets.
fn read_table_entries(
    rom: &dyn ReadOnlyMemory,
    base: u64,
    stride: u64,
    count: u64,
    entry_size: usize,
) -> Option<Vec<u64>> {
    let mut targets = Vec::with_capacity(count as usize);
    for i in 0..count {
        // Address = base + i*stride.  Use saturating math: a wrap
        // here would mean the table runs past u64::MAX, which is
        // physically impossible on any real arch — return None to
        // be safe.
        let offset = i.checked_mul(stride)?;
        let addr = base.checked_add(offset)?;
        // Use VnSpace::RAM as the read space.  CORRECTNESS: jump
        // tables live in the loaded image's read-only data
        // (`.rodata`) or sometimes `.text`; both are addressable
        // through `VnSpace::RAM` from the IR side regardless of
        // the Load's literal `space` field, because the
        // ElfFileMemReader's ReadOnlyMemory impl reads through the
        // address-space-agnostic loaded-segments map.
        let value = rom.read(VnSpace::RAM, addr, entry_size)?;
        targets.push(value);
    }
    Some(targets)
}

// Note: `NodeOutputType` import is used inside #[cfg(test)] only; keep
// it visible at the top of the file via `use ir::node::NodeOutputType`.
#[allow(dead_code)]
fn _keep_node_output_type_import_alive() {
    let _ = NodeOutputType::U32;
}

#[cfg(test)]
mod tests {
    //! Unit tests for the jump-table classifier.
    //!
    //! Each test builds a minimal [`BuiltFunctionGraph`] via
    //! [`ir::FunctionBuilder::new_raw`] (and `graph.create_node` for
    //! shapes the validator otherwise rejects), then invokes the
    //! piece-under-test in isolation.  Helpers are scoped to the
    //! module rather than promoted to `tier2_helpers.rs` so the
    //! unit tests stay self-contained.

    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, dead_code)]

    use super::*;
    use ir::FunctionBuilder;
    use ir::node::NodeOutputType;
    use std::sync::Mutex;

    /// Toy `ReadOnlyMemory` impl that returns successive 4-byte
    /// values at `base`, `base + stride`, `base + 2*stride`, …
    /// according to a fixed table.  Reads outside the table return
    /// None.  Used to exercise `read_table_entries` deterministically
    /// and to drive the integration tests' rom setup.
    pub struct TableRom {
        pub base: u64,
        pub stride: u64,
        pub entries: Vec<u64>,
        pub size: usize,
    }

    impl ReadOnlyMemory for TableRom {
        fn read(&self, _space: VnSpace, addr: u64, size: usize) -> Option<u64> {
            if size != self.size {
                return None;
            }
            if addr < self.base {
                return None;
            }
            let offset = addr - self.base;
            if self.stride == 0 {
                return None;
            }
            if !offset.is_multiple_of(self.stride) {
                return None;
            }
            let idx = (offset / self.stride) as usize;
            self.entries.get(idx).copied()
        }
    }

    /// `ReadOnlyMemory` impl that records every (addr,size) read it
    /// services.  Used to assert `read_table_entries` issues exactly
    /// `count` reads in index order.
    pub struct RecordingRom {
        pub inner: TableRom,
        pub log: Mutex<Vec<(u64, usize)>>,
    }

    impl ReadOnlyMemory for RecordingRom {
        fn read(&self, space: VnSpace, addr: u64, size: usize) -> Option<u64> {
            self.log.lock().unwrap().push((addr, size));
            self.inner.read(space, addr, size)
        }
    }

    /// `ReadOnlyMemory` impl that reads `cutoff` entries successfully
    /// then returns None for the rest.  Drives the partial-read
    /// soundness test.
    pub struct PartialRom {
        pub inner: TableRom,
        pub cutoff: usize,
    }

    impl ReadOnlyMemory for PartialRom {
        fn read(&self, space: VnSpace, addr: u64, size: usize) -> Option<u64> {
            if addr < self.inner.base {
                return None;
            }
            let offset = addr - self.inner.base;
            if self.inner.stride == 0 {
                return None;
            }
            let idx = (offset / self.inner.stride) as usize;
            if idx >= self.cutoff {
                return None;
            }
            self.inner.read(space, addr, size)
        }
    }

    /// Minimal `BuiltFunctionGraph` carrying nothing but the entry
    /// region terminated by a placeholder `Return(anchor)`.  The
    /// caller-supplied closure builds the anchor's producer subtree.
    fn build_with_anchor(
        anchor_inputs: impl FnOnce(&mut FunctionBuilder) -> NodeOutputId,
    ) -> (BuiltFunctionGraph, NodeOutputId) {
        let mut builder = FunctionBuilder::new_raw(vec![], &[], &[], &[], None, 0)
            .expect("FunctionBuilder::new_raw");
        let region = builder.create_region().expect("create_region");
        builder.set_entry_region(region).expect("set_entry_region");
        builder.set_region(region);
        let anchor = anchor_inputs(&mut builder);
        builder.build_return(Some(anchor), &[]).expect("build_return");
        let graph = builder.build().expect("build");
        (graph, anchor)
    }

    /// Builds `Load[ IntAdd( IntConst(base), IntMul(idx, IntConst(stride)) ) ]`
    /// where `idx` is provided by the closure.  Used by several shape
    /// tests.
    fn build_jt_load(
        base: u64,
        stride: u64,
        commute_add: bool,
        commute_mul: bool,
        idx_provider: impl FnOnce(&mut FunctionBuilder) -> NodeOutputId,
    ) -> (BuiltFunctionGraph, NodeOutputId) {
        build_with_anchor(|fb| {
            let idx = idx_provider(fb);
            let stride_c = fb.build_int_const(stride, NodeOutputType::U32);
            let mul = if commute_mul {
                fb.build_int_binary_operation(stride_c, idx, IntBinaryOp::Mul, NodeOutputType::U32)
                    .expect("mul")
            } else {
                fb.build_int_binary_operation(idx, stride_c, IntBinaryOp::Mul, NodeOutputType::U32)
                    .expect("mul")
            };
            let base_c = fb.build_int_const(base, NodeOutputType::U32);
            let addr = if commute_add {
                fb.build_int_binary_operation(mul, base_c, IntBinaryOp::Add, NodeOutputType::U32)
                    .expect("add")
            } else {
                fb.build_int_binary_operation(base_c, mul, IntBinaryOp::Add, NodeOutputType::U32)
                    .expect("add")
            };
            fb.build_load(addr, VnSpace::RAM, NodeOutputType::U32)
                .expect("load")
        })
    }

    // ── Shape-match tests ────────────────────────────────────────────────────

    /// Build a non-IntConst integer value usable as `idx` for the
    /// shape tests.  We need a producer that ISN'T an IntConst so
    /// `match_jump_table_shape` can disambiguate `idx` from
    /// `IntConst(stride)` in commuted multiplications — otherwise
    /// both mul operands are IntConsts and the matcher picks the
    /// wrong "stride".
    fn build_non_const_idx(fb: &mut FunctionBuilder) -> NodeOutputId {
        let addr = fb.build_int_const(0x9000u64, NodeOutputType::U32);
        fb.build_load(addr, VnSpace::RAM, NodeOutputType::U32)
            .expect("u32 load (idx)")
    }

    #[test]
    fn match_jump_table_shape_recognises_canonical_form() {
        // Load[base + idx*stride], non-commuted variant.  idx is a
        // load (non-const) so the shape match's stride-vs-idx
        // disambiguation is exercised cleanly.
        let (g, anchor) = build_jt_load(0x4000, 4, false, false, build_non_const_idx);
        let shape = match_jump_table_shape(&g, anchor).expect("must match");
        assert_eq!(shape.base, 0x4000);
        assert_eq!(shape.stride, 4);
        assert_eq!(shape.entry_size, 4);
    }

    #[test]
    fn match_jump_table_shape_recognises_commuted_intadd() {
        // IntAdd(IntMul(idx, stride), IntConst(base)) — base on the
        // right.  match-shape must try both orderings.
        let (g, anchor) = build_jt_load(0x5000, 4, true, false, build_non_const_idx);
        let shape = match_jump_table_shape(&g, anchor).expect("must match commuted add");
        assert_eq!(shape.base, 0x5000);
        assert_eq!(shape.stride, 4);
    }

    #[test]
    fn match_jump_table_shape_recognises_commuted_intmul() {
        // IntMul(IntConst(stride), idx) — stride on the left of the
        // multiplication.
        let (g, anchor) = build_jt_load(0x6000, 8, false, true, build_non_const_idx);
        let shape = match_jump_table_shape(&g, anchor).expect("must match commuted mul");
        assert_eq!(shape.base, 0x6000);
        assert_eq!(shape.stride, 8);
    }

    #[test]
    fn match_jump_table_shape_recognises_both_commutations() {
        // Both add and mul commuted — the worst-case ordering.
        let (g, anchor) = build_jt_load(0x7000, 4, true, true, build_non_const_idx);
        let shape = match_jump_table_shape(&g, anchor).expect("must match both commuted");
        assert_eq!(shape.base, 0x7000);
        assert_eq!(shape.stride, 4);
    }

    #[test]
    fn match_jump_table_shape_rejects_non_load_producer() {
        // Anchor is a raw IntConst, not a Load.  Reject.
        let (g, anchor) = build_with_anchor(|fb| fb.build_int_const(0x1000u64, NodeOutputType::U32));
        assert!(match_jump_table_shape(&g, anchor).is_none());
    }

    #[test]
    fn match_jump_table_shape_rejects_load_with_unrelated_addr_shape() {
        // Load[IntConst(addr)] — a simple global read, no Add/Mul.
        // Our shape requires IntAdd at the top of the address tree.
        let (g, anchor) = build_with_anchor(|fb| {
            let addr = fb.build_int_const(0x1234u64, NodeOutputType::U32);
            fb.build_load(addr, VnSpace::RAM, NodeOutputType::U32).expect("load")
        });
        assert!(match_jump_table_shape(&g, anchor).is_none());
    }

    #[test]
    fn match_jump_table_shape_rejects_load_without_intconst_base() {
        // Load[ IntAdd( idx_or_some_var, IntMul(idx, stride) ) ] where
        // the "base" side is not a constant.  We reject because we
        // can't pin table[0]'s address without a const base.
        //
        // Build: anchor = Load[IntAdd(IntMul(idx, 4), IntMul(idx, 4))]
        // — both add operands are mul-shaped, neither is an IntConst.
        let (g, anchor) = build_with_anchor(|fb| {
            let idx = fb.build_int_const(2u64, NodeOutputType::U32);
            let stride_c = fb.build_int_const(4u64, NodeOutputType::U32);
            let mul1 = fb
                .build_int_binary_operation(idx, stride_c, IntBinaryOp::Mul, NodeOutputType::U32)
                .expect("mul1");
            let mul2 = fb
                .build_int_binary_operation(idx, stride_c, IntBinaryOp::Mul, NodeOutputType::U32)
                .expect("mul2");
            let addr = fb
                .build_int_binary_operation(mul1, mul2, IntBinaryOp::Add, NodeOutputType::U32)
                .expect("add");
            fb.build_load(addr, VnSpace::RAM, NodeOutputType::U32).expect("load")
        });
        assert!(match_jump_table_shape(&g, anchor).is_none());
    }

    // ── Bound-via-known-bits tests ───────────────────────────────────────────

    #[test]
    fn bound_via_known_bits_returns_max_plus_one() {
        // idx = (some_var) & 0x7 → bound = 8.
        let (g, idx) = build_with_anchor(|fb| {
            let v = fb.build_int_const(0xffff_ffffu64, NodeOutputType::U32);
            let mask = fb.build_int_const(0x7u64, NodeOutputType::U32);
            fb.build_int_binary_operation(v, mask, IntBinaryOp::And, NodeOutputType::U32)
                .expect("and")
        });
        let bound = bound_via_known_bits(&g, idx).expect("must bound");
        assert_eq!(bound, 8);
    }

    #[test]
    fn bound_via_known_bits_returns_none_when_unbounded() {
        // idx = some unbounded U32 (a load output, no AND mask) → None.
        let (g, idx) = build_with_anchor(|fb| {
            let addr = fb.build_int_const(0x1000u64, NodeOutputType::U32);
            fb.build_load(addr, VnSpace::RAM, NodeOutputType::U32).expect("load")
        });
        assert_eq!(bound_via_known_bits(&g, idx), None);
    }

    #[test]
    fn bound_via_known_bits_with_int_const_input() {
        // idx = IntConst(5) directly.  KnownBits gives mask = 5,
        // bound = 6.  (Real graphs would have ConstantFold collapse
        // this to a Single, but the local recurrence handles it
        // anyway.)
        let (g, idx) = build_with_anchor(|fb| fb.build_int_const(5u64, NodeOutputType::U32));
        let bound = bound_via_known_bits(&g, idx).expect("must bound a const");
        assert_eq!(bound, 6);
    }

    #[test]
    fn bound_via_known_bits_handles_zero_extend() {
        // idx = ZeroExtend(u8 value).  Bound = 256 from the
        // narrower-type mask, regardless of the wider U32's full
        // range.  Build by hand via Graph::create_node because the
        // public `extend_if_needed` short-circuits constant inputs
        // to a folded IntConst, defeating the test's purpose.
        use ir::node::NodeOutputKind;
        let mut builder = FunctionBuilder::new_raw(vec![], &[], &[], &[], None, 0).unwrap();
        let region = builder.create_region().unwrap();
        builder.set_entry_region(region).unwrap();
        builder.set_region(region);
        // We need a non-IntConst U8 producer to feed into the Extend.
        // Use a U32 load truncated to U8 — both built via create_node
        // so we don't depend on builder's truncate-fold path.
        // Simpler: build a Load that produces U8.
        let addr = builder.build_int_const(0x9000u64, NodeOutputType::U32);
        let narrow = builder
            .build_load(addr, VnSpace::RAM, NodeOutputType::U8)
            .expect("u8 load");
        // Build the Extend node directly so it isn't folded.
        let mut g = builder.build().expect("build");
        let extend_node = g.graph.create_node(
            NodeKind::Extend(ir::ExtendOp::ZeroExtend),
            [narrow],
            [NodeOutputKind::OutputType(NodeOutputType::U32)],
        );
        let [idx] = g
            .graph
            .node_outputs_exact::<1>(extend_node)
            .expect("extend output");
        let bound = bound_via_known_bits(&g, idx).expect("bound from zero-extend");
        // U8 narrows to 0..255, so bound = 256.
        assert_eq!(bound, 256);
    }

    // ── Read-table-entries tests ─────────────────────────────────────────────

    #[test]
    fn read_table_entries_returns_targets_in_index_order() {
        // 4 entries: 0x100, 0x200, 0x300, 0x400.  Stride 4, base
        // 0x4000.  Verify the returned vec preserves index order.
        let rom = TableRom {
            base: 0x4000,
            stride: 4,
            entries: vec![0x100, 0x200, 0x300, 0x400],
            size: 4,
        };
        let result = read_table_entries(&rom, 0x4000, 4, 4, 4).expect("must read all");
        assert_eq!(result, vec![0x100, 0x200, 0x300, 0x400]);
    }

    #[test]
    fn read_table_entries_returns_none_on_partial_read() {
        // 4 entries requested; rom only serves the first 2.  Must
        // fail closed: returns None, NOT a Vec of length 2.
        let rom = PartialRom {
            inner: TableRom {
                base: 0x5000,
                stride: 4,
                entries: vec![0x100, 0x200, 0x300, 0x400],
                size: 4,
            },
            cutoff: 2,
        };
        assert_eq!(read_table_entries(&rom, 0x5000, 4, 4, 4), None);
    }

    #[test]
    fn read_table_entries_issues_count_reads_in_index_order() {
        // RecordingRom logs every (addr, size) pair.  For 3 entries
        // at stride 4, base 0x6000, expect: (0x6000, 4), (0x6004, 4),
        // (0x6008, 4) in that order.
        let rom = RecordingRom {
            inner: TableRom {
                base: 0x6000,
                stride: 4,
                entries: vec![0xaaaa, 0xbbbb, 0xcccc],
                size: 4,
            },
            log: Mutex::new(Vec::new()),
        };
        let _ = read_table_entries(&rom, 0x6000, 4, 3, 4).expect("read");
        let log = rom.log.lock().unwrap().clone();
        assert_eq!(log, vec![(0x6000, 4), (0x6004, 4), (0x6008, 4)]);
    }

    // ── End-to-end classifier-on-shape tests ────────────────────────────────

    #[test]
    fn classify_jump_table_with_known_bits_bound_returns_multiple() {
        // idx = (load) & 0x7 → bound 8.
        // Load[base + idx*stride] → resolves to Multiple of
        // table[0..8].
        let (g, anchor) = build_with_anchor(|fb| {
            // idx side: AND-masked to 0..7.
            let raw = fb.build_int_const(0xffff_ffffu64, NodeOutputType::U32);
            let mask = fb.build_int_const(0x7u64, NodeOutputType::U32);
            let idx = fb
                .build_int_binary_operation(raw, mask, IntBinaryOp::And, NodeOutputType::U32)
                .expect("and");
            let stride_c = fb.build_int_const(4u64, NodeOutputType::U32);
            let mul = fb
                .build_int_binary_operation(idx, stride_c, IntBinaryOp::Mul, NodeOutputType::U32)
                .expect("mul");
            let base_c = fb.build_int_const(0x4000u64, NodeOutputType::U32);
            let addr = fb
                .build_int_binary_operation(base_c, mul, IntBinaryOp::Add, NodeOutputType::U32)
                .expect("add");
            fb.build_load(addr, VnSpace::RAM, NodeOutputType::U32)
                .expect("load")
        });
        let rom = TableRom {
            base: 0x4000,
            stride: 4,
            entries: vec![0x10, 0x20, 0x30, 0x40, 0x50, 0x60, 0x70, 0x80],
            size: 4,
        };
        let result = classify_jump_table(&g, anchor, Some(&rom), None);
        match result {
            Some(ResolvedTargets::Multiple(ts)) => {
                assert_eq!(ts, vec![0x10, 0x20, 0x30, 0x40, 0x50, 0x60, 0x70, 0x80]);
            }
            other => panic!("expected Multiple([0x10..0x80]); got {other:?}"),
        }
    }

    #[test]
    fn classify_jump_table_no_rom_returns_none() {
        // Bounded shape, but no rom configured → None.  Without rom
        // we can't read entries, and producing a Multiple without
        // entries is unsound.
        let (g, anchor) = build_with_anchor(|fb| {
            let raw = fb.build_int_const(0xffff_ffffu64, NodeOutputType::U32);
            let mask = fb.build_int_const(0x3u64, NodeOutputType::U32);
            let idx = fb
                .build_int_binary_operation(raw, mask, IntBinaryOp::And, NodeOutputType::U32)
                .expect("and");
            let stride_c = fb.build_int_const(4u64, NodeOutputType::U32);
            let mul = fb
                .build_int_binary_operation(idx, stride_c, IntBinaryOp::Mul, NodeOutputType::U32)
                .expect("mul");
            let base_c = fb.build_int_const(0x4000u64, NodeOutputType::U32);
            let addr = fb
                .build_int_binary_operation(base_c, mul, IntBinaryOp::Add, NodeOutputType::U32)
                .expect("add");
            fb.build_load(addr, VnSpace::RAM, NodeOutputType::U32).expect("load")
        });
        let result = classify_jump_table(&g, anchor, None, None);
        assert_eq!(result, None);
    }

    #[test]
    fn classify_jump_table_unbounded_idx_returns_none() {
        // Shape is jt-shaped, but `idx` is a raw load with no AND
        // mask; predecessor-If walk also can't bound it (no If on
        // the path).  Must return None, not a Multiple.
        let (g, anchor) = build_with_anchor(|fb| {
            let some_addr = fb.build_int_const(0x9000u64, NodeOutputType::U32);
            let idx = fb
                .build_load(some_addr, VnSpace::RAM, NodeOutputType::U32)
                .expect("load idx");
            let stride_c = fb.build_int_const(4u64, NodeOutputType::U32);
            let mul = fb
                .build_int_binary_operation(idx, stride_c, IntBinaryOp::Mul, NodeOutputType::U32)
                .expect("mul");
            let base_c = fb.build_int_const(0x4000u64, NodeOutputType::U32);
            let addr = fb
                .build_int_binary_operation(base_c, mul, IntBinaryOp::Add, NodeOutputType::U32)
                .expect("add");
            fb.build_load(addr, VnSpace::RAM, NodeOutputType::U32).expect("load")
        });
        let rom = TableRom {
            base: 0x4000,
            stride: 4,
            entries: vec![0x10, 0x20, 0x30, 0x40],
            size: 4,
        };
        let result = classify_jump_table(&g, anchor, Some(&rom), None);
        assert_eq!(result, None);
    }

    // ── bound_from_if_condition unit tests (direct) ─────────────────────────

    #[test]
    fn bound_from_if_condition_idx_less_than_n_true() {
        // Build idx and an `IntCmpOp::Less(idx, IntConst(4))`.  The
        // helper is on the `on_true` branch → bound = 4.
        let mut builder = FunctionBuilder::new_raw(vec![], &[], &[], &[], None, 0).unwrap();
        let region = builder.create_region().unwrap();
        builder.set_entry_region(region).unwrap();
        builder.set_region(region);
        let idx = builder.build_int_const(0u64, NodeOutputType::U32);
        let n = builder.build_int_const(4u64, NodeOutputType::U32);
        let cmp = builder
            .build_int_cmp_operation(idx, n, IntCmpOp::Less, NodeOutputType::U32)
            .unwrap();
        // Anchor with a placeholder return so build() succeeds.
        builder.build_return(Some(idx), &[]).unwrap();
        let g = builder.build().unwrap();
        let bound = bound_from_if_condition(&g, cmp, idx, /* on_true */ true);
        assert_eq!(bound, Some(4));
    }

    #[test]
    fn bound_from_if_condition_idx_less_than_n_false_returns_none() {
        // Same shape, but on the false branch → no upper bound.
        let mut builder = FunctionBuilder::new_raw(vec![], &[], &[], &[], None, 0).unwrap();
        let region = builder.create_region().unwrap();
        builder.set_entry_region(region).unwrap();
        builder.set_region(region);
        let idx = builder.build_int_const(0u64, NodeOutputType::U32);
        let n = builder.build_int_const(4u64, NodeOutputType::U32);
        let cmp = builder
            .build_int_cmp_operation(idx, n, IntCmpOp::Less, NodeOutputType::U32)
            .unwrap();
        builder.build_return(Some(idx), &[]).unwrap();
        let g = builder.build().unwrap();
        let bound = bound_from_if_condition(&g, cmp, idx, /* on_true */ false);
        assert_eq!(bound, None);
    }

    #[test]
    fn bound_from_if_condition_idx_le_n_true_is_n_plus_one() {
        // idx <= 4 (taken-true) → bound = 5.
        let mut builder = FunctionBuilder::new_raw(vec![], &[], &[], &[], None, 0).unwrap();
        let region = builder.create_region().unwrap();
        builder.set_entry_region(region).unwrap();
        builder.set_region(region);
        let idx = builder.build_int_const(0u64, NodeOutputType::U32);
        let n = builder.build_int_const(4u64, NodeOutputType::U32);
        let cmp = builder
            .build_int_cmp_operation(idx, n, IntCmpOp::LessEqual, NodeOutputType::U32)
            .unwrap();
        builder.build_return(Some(idx), &[]).unwrap();
        let g = builder.build().unwrap();
        let bound = bound_from_if_condition(&g, cmp, idx, true);
        assert_eq!(bound, Some(5));
    }

    /// Helper: build a graph where `entry` branches via
    /// `if (idx < bound) { dispatch } else { exit }`, and the
    /// dispatch region's placeholder Return uses an
    /// `idx_in_dispatch` value (the dispatch's read of the same
    /// idx_var, which travels through a single-input ControlPhi).
    /// Returns the graph, the anchor (placeholder Return's
    /// value-input), and the dispatch's view of idx.
    fn build_pred_if_graph(
        bound: u64,
    ) -> (BuiltFunctionGraph, NodeOutputId, NodeOutputId) {
        use ir::{FunctionBuilder, IntCmpOp};
        let idx_var = rsleigh::Vn {
            addr: rsleigh::VnAddr {
                space: rsleigh::VnSpace::REGISTER,
                off: 0x10,
            },
            size: 4,
        };
        let mut b = FunctionBuilder::new_raw(vec![idx_var], &[], &[], &[], None, 0).unwrap();
        let entry = b.create_region().unwrap();
        let dispatch = b.create_region().unwrap();
        let exit = b.create_region().unwrap();
        b.set_entry_region(entry).unwrap();

        b.set_region(entry);
        let idx_at_entry = b.read_variable(&idx_var).unwrap();
        let bound_c = b.build_int_const(bound, NodeOutputType::U32);
        let cond = b
            .build_int_cmp_operation(idx_at_entry, bound_c, IntCmpOp::Less, NodeOutputType::U32)
            .unwrap();
        b.build_if(cond, dispatch, exit).unwrap();

        b.set_region(dispatch);
        let idx_in_dispatch = b.read_variable(&idx_var).unwrap();
        // Use idx_in_dispatch as the placeholder anchor — exercises
        // the bound walk against the dispatch's own idx-output, which
        // (without RedundantPhis) wraps the entry idx in a
        // single-input ControlPhi.
        b.build_return(Some(idx_in_dispatch), &[]).unwrap();

        b.set_region(exit);
        b.build_return(None, &[]).unwrap();

        let g = b.build().unwrap();
        // The placeholder Return is the 3-input one in dispatch.
        let mut anchor = None;
        for nid in g.preorder() {
            if !matches!(g.graph.node_kind(nid), NodeKind::Return) {
                continue;
            }
            let inputs: Vec<_> = g.graph.node_inputs(nid).into_iter().collect();
            if inputs.len() == 3 {
                anchor = Some(inputs[2]);
            }
        }
        (g, anchor.expect("placeholder return"), idx_in_dispatch)
    }

    #[test]
    fn bound_via_predecessor_if_walks_one_hop() {
        // `If(idx < 4)` directly dominates the placeholder Return's
        // region.  bound_via_predecessor_if must follow control back
        // through one hop and surface bound = 4.
        let (g, anchor, idx_in_dispatch) = build_pred_if_graph(4);
        let bound = bound_via_predecessor_if(&g, anchor, idx_in_dispatch);
        assert_eq!(bound, Some(4));
    }

    #[test]
    fn bound_via_predecessor_if_returns_none_when_no_if_on_path() {
        // No If on the path (single-region function with raw idx).
        // The walk reaches Entry without finding a bound → None.
        use ir::FunctionBuilder;
        let idx_var = rsleigh::Vn {
            addr: rsleigh::VnAddr {
                space: rsleigh::VnSpace::REGISTER,
                off: 0x10,
            },
            size: 4,
        };
        let mut b = FunctionBuilder::new_raw(vec![idx_var], &[], &[], &[], None, 0).unwrap();
        let region = b.create_region().unwrap();
        b.set_entry_region(region).unwrap();
        b.set_region(region);
        let idx = b.read_variable(&idx_var).unwrap();
        b.build_return(Some(idx), &[]).unwrap();
        let g = b.build().unwrap();
        let mut anchor = None;
        for nid in g.preorder() {
            if !matches!(g.graph.node_kind(nid), NodeKind::Return) {
                continue;
            }
            let inputs: Vec<_> = g.graph.node_inputs(nid).into_iter().collect();
            if inputs.len() == 3 {
                anchor = Some(inputs[2]);
            }
        }
        let anchor = anchor.expect("anchor");
        let bound = bound_via_predecessor_if(&g, anchor, idx);
        assert_eq!(bound, None);
    }

    #[test]
    fn bound_via_predecessor_if_returns_none_when_idx_unrelated_to_cond() {
        // The If's condition compares a DIFFERENT variable, not the
        // dispatch's idx.  The walk must NOT confabulate a bound.
        use ir::{FunctionBuilder, IntCmpOp};
        let idx_var = rsleigh::Vn {
            addr: rsleigh::VnAddr {
                space: rsleigh::VnSpace::REGISTER,
                off: 0x10,
            },
            size: 4,
        };
        let other_var = rsleigh::Vn {
            addr: rsleigh::VnAddr {
                space: rsleigh::VnSpace::REGISTER,
                off: 0x14,
            },
            size: 4,
        };
        let mut b = FunctionBuilder::new_raw(vec![idx_var, other_var], &[], &[], &[], None, 0)
            .unwrap();
        let entry = b.create_region().unwrap();
        let dispatch = b.create_region().unwrap();
        let exit = b.create_region().unwrap();
        b.set_entry_region(entry).unwrap();

        b.set_region(entry);
        // Compare OTHER var, not idx.
        let other = b.read_variable(&other_var).unwrap();
        let bound_c = b.build_int_const(4u64, NodeOutputType::U32);
        let cond = b
            .build_int_cmp_operation(other, bound_c, IntCmpOp::Less, NodeOutputType::U32)
            .unwrap();
        b.build_if(cond, dispatch, exit).unwrap();

        b.set_region(dispatch);
        let idx_in_dispatch = b.read_variable(&idx_var).unwrap();
        b.build_return(Some(idx_in_dispatch), &[]).unwrap();
        b.set_region(exit);
        b.build_return(None, &[]).unwrap();

        let g = b.build().unwrap();
        let mut anchor = None;
        for nid in g.preorder() {
            if !matches!(g.graph.node_kind(nid), NodeKind::Return) {
                continue;
            }
            let inputs: Vec<_> = g.graph.node_inputs(nid).into_iter().collect();
            if inputs.len() == 3 {
                anchor = Some(inputs[2]);
            }
        }
        let anchor = anchor.expect("anchor");
        let bound = bound_via_predecessor_if(&g, anchor, idx_in_dispatch);
        assert_eq!(bound, None, "If on unrelated var must not bound idx");
    }

    #[test]
    fn bound_from_if_condition_unrelated_idx_returns_none() {
        // The cmp is on `other`, not `idx`.  Must return None.
        let mut builder = FunctionBuilder::new_raw(vec![], &[], &[], &[], None, 0).unwrap();
        let region = builder.create_region().unwrap();
        builder.set_entry_region(region).unwrap();
        builder.set_region(region);
        let idx = builder.build_int_const(0u64, NodeOutputType::U32);
        let other = builder.build_int_const(7u64, NodeOutputType::U32);
        let n = builder.build_int_const(4u64, NodeOutputType::U32);
        let cmp = builder
            .build_int_cmp_operation(other, n, IntCmpOp::Less, NodeOutputType::U32)
            .unwrap();
        builder.build_return(Some(idx), &[]).unwrap();
        let g = builder.build().unwrap();
        let bound = bound_from_if_condition(&g, cmp, idx, true);
        assert_eq!(bound, None);
    }
}
