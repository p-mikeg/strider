//! Jump-table arm for the indirect-branch classifier.
//!
//! Recognises the canonical jump-table dispatch
//! shape — `Load(IntAdd(IntConst(base), IntMul(idx, IntConst(stride))))`
//! and its commutative variants, plus the AArch64-style scaled-index
//! form `Load(IntAdd(IntConst(base), Shl(idx, IntConst(shift))))`
//! (equivalent to `stride = 1 << shift`) — proves an upper bound `N`
//! on the index `idx`, reads the `N` table entries from a
//! caller-supplied [`ReadOnlyMemory`], and returns
//! [`ResolvedTargets::Multiple([table[0], …, table[N-1]])`].
//!
//! ## Soundness
//!
//! Two independent gates must hold for the classifier to commit to
//! `Multiple`:
//!
//! 1. **Bounded index.**  Either [`crate::KnownBits`] proves `idx`'s upper
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

use super::MAX_TABLE_ENTRIES;
use crate::ReadOnlyMemory;
#[cfg(test)]
use rsleigh::VnSpace;
use strider_ir::node::{NodeId, NodeKind, ValueId};
use strider_ir::{Graph, IntCmpOp};
use strider_lift::cfg::ResolvedTargets;

/// Top-level classifier hook for the jump-table arm.  Called by
/// [`super::classify::classify_anchor`] when the anchor's producer is
/// a [`NodeKind::Load`].
///
/// `anchor_value` is the placeholder Return's value-input slot.
/// `rom` is the read-only memory image — almost always the ELF's
/// `.rodata` + `.text` view for callers that load real binaries.
#[must_use]
pub fn classify_jump_table(
    ctx: crate::RewriteCtxView<'_>,
    anchor_value: ValueId,
    rom: Option<&dyn ReadOnlyMemory>,
    endianness: strider_target::Endianness,
    known: &crate::KnownBitsMap,
) -> Option<ResolvedTargets> {
    // Structural shape match.  `match_jump_table_shape` returns the
    // `idx` value and the `(base, stride, entry_size)` triple —
    // everything we need to enumerate entries.  Falls through to None
    // for every shape that isn't an honest jump-table dispatch.
    let shape = match_jump_table_shape(ctx, anchor_value)?;

    // Bound the index.  Two strategies, tried in order:
    //   (a) KnownBits — purely structural inspection of the IR;
    //       cheap; works whenever the shape contains an explicit
    //       AND-mask (`idx & 0x7` etc.).
    //   (b) Predecessor-If walk — looks for an `If(idx < N)` on the
    //       control path leading to the dispatch's region.  Slower
    //       but covers the gcc-emitted "compare-and-branch then
    //       indirect" pattern that has no AND-mask.
    let bound = bound_via_known_bits(ctx, shape.idx_value, known)
        .or_else(|| bound_via_predecessor_if(ctx, anchor_value, shape.idx_value, known))?;

    // Enforce the per-call enumeration cap.  Returning None here is
    // sound: the orchestrator will defer; if a future iteration
    // tightens the bound (e.g. PhiCollapse exposes a narrower
    // KnownBits result) the table will resolve.
    if bound == 0 || bound > MAX_TABLE_ENTRIES {
        return None;
    }

    // Read the table.  Failing closed (None on partial read) is the
    // soundness guard: a partial Multiple would omit valid runtime
    // targets and the orchestrator would wire a CFG missing those
    // edges.  See `read_table_entries` for the full rule.
    let rom = rom?;
    let targets = read_table_entries(
        rom,
        shape.base,
        shape.stride,
        bound,
        shape.entry_size,
        endianness,
    )?;

    // Sort + dedup so the resulting Multiple is canonical (matches
    // the orchestrator's edge-set comparison protocol — see the
    // anonymous `Phi` arm in classify.rs for the same rationale).
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
    idx_value: ValueId,
    /// The Load's per-entry size in bytes (matches the Load's output
    /// type).  Distinct from `stride` because some tables have
    /// padding between entries (`stride > entry_size`); we read
    /// `entry_size` bytes at each `base + i * stride`.
    entry_size: usize,
}

/// Recognises the canonical jump-table address shape on the producer
/// of `anchor_value`.
///
/// Accepted shapes:
///
/// **Multiplicative (gcc / clang on x86, scale via `lea`'s SIB byte;
/// commutativity of `+` and `*` is honoured):**
///   * `Load[ IntAdd( IntConst(base), IntMul(idx,        IntConst(stride)) ) ]`
///   * `Load[ IntAdd( IntConst(base), IntMul(IntConst(stride), idx       ) ) ]`
///   * `Load[ IntAdd( IntMul(idx,        IntConst(stride)), IntConst(base) ) ]`
///   * `Load[ IntAdd( IntMul(IntConst(stride), idx       ), IntConst(base) ) ]`
///
/// **Shift-scaled (AArch64 `LDR Xn, [Xb, Xi, LSL #N]`, ARM
/// `LDR Rn, [Rb, Ri, LSL #N]`; `Shl` is non-commutative so only `+`
/// is auto-commuted):**
///   * `Load[ IntAdd( IntConst(base), Shl(idx, IntConst(shift)) ) ]`
///   * `Load[ IntAdd( Shl(idx, IntConst(shift)), IntConst(base) ) ]`
///
/// where the effective stride is `1 << shift`.  `shift` must be in
/// `0..64` so the implied stride fits in `u64`; the sane real-world
/// values are `0..=3` (entry sizes 1/2/4/8 bytes).
///
/// Every other shape — including degenerate `Load[IntConst(addr)]`
/// (a simple global read; the `IntConst` arm in `classify.rs` would
/// handle that one if it were a dispatch target) — returns None and
/// defers to whatever later arms exist.
///
/// ## Bound caveat
///
/// Matching the shape is only half the work — the classifier still
/// needs an upper bound `N` on `idx`.  AArch64 `cmp + b.hi` lifts to a
/// flag-based boolean expression (`!Z & C`) rather than a direct
/// `IntCmpOp::Less(idx, N)`, so [`bound_via_predecessor_if`] currently
/// can't recover the bound for that pattern.  The shape match still
/// fires, but resolution falls through to
/// [`crate::error::Error`] / `UnresolvedIndirectBranch` until the
/// bound walker grows flag-based-cmp support.  Tables with a bound
/// the analyser CAN see (an explicit `idx & MASK` AND-mask, an x86
/// `cmp + jb`/`ja` that lifts to `Less`/`LessEqual` directly, or a
/// signed `cmp + b.lt` on AArch64) resolve normally.
fn match_jump_table_shape(
    ctx: crate::RewriteCtxView<'_>,
    anchor_value: ValueId,
) -> Option<JumpTableShape> {
    let graph = ctx.graph_ref();
    // The producer must be a Load.  classify.rs already routes here
    // only on Load, but we re-check defensively so this function is
    // testable in isolation.  We pull `space` and `entry_size` off the
    // matched node up-front; the pattern-DSL match below then handles
    // the structural shape only.
    let load_node = graph.producer(anchor_value);
    let NodeKind::Load(_space) = *graph.node_kind(load_node) else {
        return None;
    };
    // Load output's type tells us the per-entry byte size; a `Load` always
    // produces a value output (validated signature).  Reject narrow/wide
    // non-pointer widths below.
    let ty = graph
        .value_kind(anchor_value)
        .as_value()
        .expect("Load output is a value");
    if !ty.is_integer() {
        return None;
    }
    let entry_size = ty.byte_size();
    // A jump-table entry is a machine pointer (≤ 8 bytes).  Wide integer
    // loads (I80/I128/I256/I512) are not table entries; reject them
    // explicitly rather than relying on the downstream `ReadOnlyMemory::read`
    // size>8 rejection to fail closed.
    if entry_size > 8 {
        return None;
    }

    // CORRECTNESS — pattern-DSL form is sound-equivalent to the four
    // hand-written commutativity cases the prior version expanded:
    // `strider_pattern::add` and `strider_pattern::mul` are auto-commutative, so the
    // single `load().addr(add(any_int_const().capture(base), mul(var(idx),
    // any_int_const().capture(stride))))` pattern matches all four operand
    // orderings of `(base + idx*stride)` without an explicit fallback
    // chain.  `any_int_const().capture(c)` guarantees the captured side is an
    // `IntConst` node, so on a successful match `idx_value` is
    // necessarily the *other* operand of the multiplication — the
    // same disambiguation the prior `extract_base_and_mul` performed
    // by trying `int_const_val` on each `mul` operand in turn.
    use strider_pattern::{Capture, CaptureExt, add, any_int_const, load, mul, shl, var};
    let base_var = Capture::new();
    let stride_var = Capture::new();
    let idx_var = Capture::new();

    // 1) Multiplicative form (x86 / `Mul`-based scaling).
    let mul_pat = load()
        .addr(add(
            any_int_const().capture(base_var),
            mul(var(idx_var), any_int_const().capture(stride_var)),
        ))
        .build();
    if let Some(m) = ctx.matcher().match_at(load_node, &mul_pat).expect("classifier pattern is single-rooted") {
        // `get_uint` returns `Option<u128>`; jump-table bases / strides
        // are addresses + element widths and must fit in u64 on every
        // supported arch.  A wide value here is a malformed match —
        // defer rather than silently routing through a truncated wrong
        // address.
        let base = crate::indirect_branch_resolve::u128_to_branch_target(
            m.bindings().get_uint(base_var, ctx.graph_ref())?,
        )?;
        let stride = crate::indirect_branch_resolve::u128_to_branch_target(
            m.bindings().get_uint(stride_var, ctx.graph_ref())?,
        )?;
        let idx_value = m.value(idx_var)?;
        return Some(JumpTableShape {
            base,
            stride,
            idx_value,
            entry_size,
        });
    }

    // 2) Shift-scaled form (AArch64 / ARM `LSL #N` addressing mode).
    // `shl` is *not* commutative — the shift amount can only sit on the
    // right-hand side — so we don't get the auto-commutativity that
    // `mul` provides; the `add` wrapping each form is still commuted
    // automatically, which gives us the two `(base, idx<<shift)` /
    // `(idx<<shift, base)` orderings for free.
    let shl_pat = load()
        .addr(add(
            any_int_const().capture(base_var),
            shl(var(idx_var), any_int_const().capture(stride_var)),
        ))
        .build();
    let m = ctx.matcher().match_at(load_node, &shl_pat).expect("classifier pattern is single-rooted")?;
    let base = crate::indirect_branch_resolve::u128_to_branch_target(
        m.bindings().get_uint(base_var, ctx.graph_ref())?,
    )?;
    let shift = m.bindings().get_uint(stride_var, ctx.graph_ref())?;
    // Reject shift amounts >= 64 — the implied stride `1u64 << shift`
    // would overflow / be UB in Rust.  Real jump-table entries are at
    // most 8 bytes (shift ≤ 3); anything larger is almost certainly a
    // mis-classification rather than a valid table.
    if shift >= 64 {
        return None;
    }
    let stride = 1u64 << shift;
    let idx_value = m.value(idx_var)?;
    Some(JumpTableShape {
        base,
        stride,
        idx_value,
        entry_size,
    })
}

// ── Bound via KnownBits ──────────────────────────────────────────────────────

/// Returns an upper bound on `idx_value`'s runtime value, derived from the
/// crate-shared [`opt::analyze_known_bits`](crate::analyze_known_bits)
/// fixed-point analyzer.
///
/// Semantics: if the analyzer proves bit `i` of `idx_value` is always
/// zero, the runtime value cannot have that bit set.  The maximum value is
/// therefore `(!zeros) & type_mask`, and the count of distinct values in
/// `[0, max]` is `max + 1`.  Returns `Some(max + 1)` whenever the analyzer
/// proves at least one upper bit is known zero; otherwise `None` so the
/// caller's predecessor-If fallback gets a chance.
///
/// Replaces a previous local recurrence that re-implemented a stripped-down
/// version of the lifter's `IntConst` / `And` / `Truncate` /
/// `ZeroExtend` / `ShiftRight` rules.  The fixed-point analyzer covers
/// every node kind those rules covered — and several more (`Or`, `Xor`,
/// `Not`, `Popcount`, `Lzcount`, `ShiftLeft`) — so any bound this function
/// previously returned is still proved, and some previously-unbounded
/// shapes now resolve.
///
/// `known` is the pre-computed result of [`crate::analyze_known_bits`] —
/// callers compute it once per resolver invocation and thread it through
/// every classified anchor so we don't re-run the worklist analysis per
/// anchor.
#[must_use]
pub(super) fn bound_via_known_bits(
    ctx: crate::RewriteCtxView<'_>,
    idx_value: ValueId,
    known: &crate::KnownBitsMap,
) -> Option<u64> {
    // Output type: only integer-typed indices make sense as table
    // indices.  Reject everything else (Bool, F32, F64, …).
    let ty = ctx.value_kind(idx_value).as_value()?;
    if !ty.is_integer() {
        return None;
    }
    // Type mask sets the maximum possible value (e.g. 0xff for I8).
    // KnownBits at most narrows below this; if no narrowing is
    // possible we return None so the predecessor-If fallback gets a
    // chance.
    let type_mask = u64::try_from(ty.get_unsigned_int(u128::from(u64::MAX))?).ok()?;

    // Outputs absent from the map have no proven bit info; treat them
    // as the all-unknown default — `SecondaryMap` returns `KnownBitsFacts::default()`
    // (the all-unknown sentinel) for unrecorded entries via `Index`.
    let kb = known[idx_value];
    let max = kb.max_value(type_mask);
    if max == type_mask {
        // No narrowing — fall back rather than try to enumerate
        // 2^bit_width entries.
        return None;
    }
    max.checked_add(1)
}

// ── Bound via predecessor-If walk ────────────────────────────────────────────

/// Walks the control-flow chain *backwards* from the placeholder
/// Return at `anchor_value`'s consumer until it finds an
/// `If(IntCmp(idx_value, IntConst(N)))` whose dominating edge is
/// the true branch.  Returns `Some(N)` when the bound is proved, or
/// None when:
///
///   * No `If` on the path tests `idx_value`.
///   * The walk reaches the function entry (no more predecessors).
///   * A multi-predecessor `Region` (a join point) is reached
///     where any incoming path doesn't have the bound.  Joining
///     mixed-bound paths fails closed: the runtime path could be
///     either, so we can't soundly assume the bound holds.
///   * A cycle is detected (back-edge of a loop).  Loops can have
///     `idx` mutated mid-iteration; our walk isn't strong enough
///     to reason about that.
///
/// CORRECTNESS: the bound from this walk is an upper bound on
/// `idx_value`'s value at the placeholder Return: every runtime
/// execution that reaches the dispatch must have traversed at
/// least one of the matched `If` edges, and on that edge the
/// `IntCmp` must have evaluated true (otherwise the false branch
/// would have been taken and we'd never reach the dispatch).
/// `IntCmp(idx, N)` evaluating true under {Less, LessEqual, Sless,
/// SlessEqual} bounds `idx` above by `N` or `N+1`.
#[must_use]
pub(super) fn bound_via_predecessor_if(
    ctx: crate::RewriteCtxView<'_>,
    anchor_value: ValueId,
    idx_value: ValueId,
    known: &crate::KnownBitsMap,
) -> Option<u64> {
    // Find the placeholder IndirectBranch that consumes the anchor.
    // This is the start of our backward walk.
    //
    // The placeholder's input slot 0 is its Control input; we walk
    // upward through Controls looking for an If whose true branch
    // leads to this placeholder.
    let graph = ctx.graph_ref();
    let placeholder = find_anchor_consumer_placeholder(graph, anchor_value)?;
    // Slot 0 = control; see node_signature::expected_signature for
    // IndirectBranch: `inputs: [CTRL, MEM, TARGET]` — guaranteed 3 inputs
    // (validated structural invariant).
    let control_value = graph
        .node_inputs_exact::<3>(placeholder)
        .expect("IndirectBranch has 3 inputs (validated)")[0];

    walk_control_for_if_bound_iter(ctx, control_value, idx_value, known)
}

/// Locates the (single) [`NodeKind::IndirectBranch`] that consumes
/// `anchor_value` — that's the placeholder the strider lift emits
/// for `UnresolvedIndirectBranch` regions.  Returns None when no
/// consumer is a placeholder — the producer-shape match should have
/// gated us out before reaching this point, so this is defensive.
fn find_anchor_consumer_placeholder(graph: &Graph, anchor_value: ValueId) -> Option<NodeId> {
    for (consumer_id, _) in graph.value_uses(anchor_value) {
        if matches!(graph.node_kind(consumer_id), NodeKind::IndirectBranch) {
            return Some(consumer_id);
        }
    }
    None
}

/// Iterative worklist version of the predecessor-If walk — same
/// behaviour as the original recursive `walk_control_for_if_bound`
/// but stack-safe at any CFG depth.  `control_value` is the
/// Control output we're currently looking at — i.e. the `Control`
/// input of whoever's downstream.  Returns the proved bound
/// (or `None`) for the path on the way to here.
///
/// **Frame model:**
/// * `Frame::Visit { control_value }` — cycle-check, classify producer,
///   either emit a result or schedule child visits.
/// * `Frame::JoinNext { … }` — at a `Region`, after each
///   predecessor's sub-walk completes, this frame pops its result,
///   rolls back the visited additions made by that pred, and either
///   schedules the next pred or finalises the join with the running
///   maximum.
///
/// The cycle-detection set + trail rollback exactly mirror the
/// recursive version: a back-edge inside one predecessor's sub-walk
/// is undone before the next predecessor starts, so the same node
/// can legitimately appear on multiple incoming paths to a join.
///
/// **Why this is not folded into a generic `walk_cfg_backward`
/// primitive in `strider_ir::walk`:** the trail-rollback semantics
/// require the verdict callback to observe and mutate the walker's
/// visited set across pred boundaries.  Exposing the visited set as a
/// trait method (so the rollback policy can be supplied by a Verdict
/// impl) is a full implementation leak — the rollback IS the walker.
/// Add to that the classifier-driven specific-predecessor selection
/// (`If` picks `inputs[0]`, default picks slot-0 if control, multi-pred
/// `Region` forks, etc.) and a hypothetical
/// `BackwardCfgVerdict` would carry the entire current body verbatim.
/// The memory-SSA walker in `crate::memory_ssa` is the right
/// abstraction for the pure-DAG / no-trail-rollback case; this walker
/// is the trail-rollback case and stays here until a second consumer
/// demonstrates the rollback policy can be parameterised cleanly.
/// JoinNext frame: the multi-predecessor `Region` is the only
/// case that needs a continuation.  Linear chains (If's transparent
/// walk, generic transparent walk, single-pred `Region`) are
/// handled by re-entering the inner loop with an updated
/// `control_value`, so they cost zero heap allocation.
struct JoinNext {
    region_node: NodeId,
    next_idx: u32,
    pre_pred_trail_len: u32,
    combined: u64,
}

/// Pushes a `JoinNext` continuation for a multi-predecessor `Region` and
/// returns the first predecessor to walk, or `None` if no inputs are found.
///
/// Extracted from [`walk_control_for_if_bound_iter`]'s Region arm to keep
/// the inner match arms at a uniform depth.
fn push_region_continuation(
    graph: &strider_ir::Graph,
    region_node: NodeId,
    trail: &[NodeId],
    work: &mut Vec<JoinNext>,
) -> Option<ValueId> {
    let pre_pred_trail_len = trail.len() as u32;
    let first_pred = graph.node_inputs(region_node).into_iter().next()?;
    work.push(JoinNext {
        region_node,
        next_idx: 1,
        pre_pred_trail_len,
        combined: 0,
    });
    Some(first_pred)
}

fn walk_control_for_if_bound_iter(
    ctx: crate::RewriteCtxView<'_>,
    initial_control_value: ValueId,
    idx_value: ValueId,
    known: &crate::KnownBitsMap,
) -> Option<u64> {
    use strider_ir::walk::NodeIdSet;

    let graph = ctx.graph_ref();
    let mut visited: NodeIdSet = NodeIdSet::new();
    let mut trail: Vec<NodeId> = Vec::new();
    // Region continuations form a stack; preallocate to avoid the first
    // few growth-realloc round trips on graphs with several joins.
    let mut work: Vec<JoinNext> = Vec::with_capacity(8);

    let mut control_value = initial_control_value;
    // Set inside the inner loop before any read in the outer loop;
    // the initial value is unused and only present so the variable
    // can be `mut` and outlive the inner loop.
    let mut last_result: Option<u64>;

    'outer: loop {
        // Linear (single-path) walk: only allocates on a multi-pred CS.
        loop {
            let producer = graph.producer(control_value);
            if visited.contains(producer) {
                // Cycle (loop back-edge): fail closed.
                last_result = None;
                break;
            }
            visited.insert(producer);
            trail.push(producer);

            match graph.node_kind(producer) {
                NodeKind::If => {
                    let (_, output_idx) = graph.value_definition(control_value);
                    // If has exactly 2 inputs [ctrl, cond] (validated
                    // structural invariant).
                    let if_inputs = graph
                        .node_inputs_exact::<2>(producer)
                        .expect("If has 2 inputs (validated)");
                    let cond_value = if_inputs[1];
                    let on_true = output_idx == 0;
                    if let Some(b) =
                        bound_from_if_condition(ctx, cond_value, idx_value, on_true, known)
                    {
                        last_result = Some(b);
                        break;
                    }
                    // No bound from this If — keep walking up.
                    control_value = if_inputs[0];
                    continue;
                }
                NodeKind::Region => {
                    let pred_count = graph.node_inputs(producer).len();
                    if pred_count == 0 {
                        last_result = None;
                        break;
                    }
                    if pred_count == 1 {
                        // Single-pred join is just a transparent walk;
                        // no rollback needed.
                        let Some(only) = graph.node_inputs(producer).into_iter().next() else {
                            last_result = None;
                            break;
                        };
                        control_value = only;
                        continue;
                    }
                    // Multi-pred: push a continuation and walk first pred.
                    let Some(first_pred) =
                        push_region_continuation(graph, producer, &trail, &mut work)
                    else {
                        last_result = None;
                        break;
                    };
                    control_value = first_pred;
                    continue;
                }
                NodeKind::Entry => {
                    last_result = None;
                    break;
                }
                _ => {
                    // Transparent walk: follow slot-0 if Control.
                    let Some(first) = graph.node_inputs(producer).into_iter().next() else {
                        last_result = None;
                        break;
                    };
                    if !graph.value_kind(first).is_control() {
                        last_result = None;
                        break;
                    }
                    control_value = first;
                    continue;
                }
            }
        }

        // Inner loop ended with a result.  Feed it to the topmost
        // pending join continuation, if any.
        loop {
            let Some(top) = work.last_mut() else {
                return last_result;
            };
            // Roll back the previous pred's contributions.
            for n in trail.drain(top.pre_pred_trail_len as usize..) {
                visited.remove(n);
            }
            match last_result {
                None => {
                    // This join fails closed; pop it and propagate.
                    work.pop();
                    last_result = None;
                    continue;
                }
                Some(b) => {
                    let new_combined = top.combined.max(b);
                    let preds = graph.node_inputs(top.region_node);
                    let pred_count = preds.len();
                    if (top.next_idx as usize) >= pred_count {
                        // All preds processed; pop this join.
                        work.pop();
                        last_result = Some(new_combined);
                        continue;
                    }
                    // Schedule next pred — update the existing frame
                    // in place (saves a push/pop pair) and re-enter
                    // the linear walk.  `next_idx < pred_count` was
                    // just checked, so `.nth` is in range; we still
                    // bail conservatively rather than `unwrap`.
                    let Some(next_pred) = graph
                        .node_inputs(top.region_node)
                        .into_iter()
                        .nth(top.next_idx as usize)
                    else {
                        work.pop();
                        last_result = None;
                        continue;
                    };
                    top.next_idx += 1;
                    top.combined = new_combined;
                    control_value = next_pred;
                    continue 'outer;
                }
            }
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
///   * `idx < N`             true → `idx <= N - 1` → bound is `N`.
///   * `idx <= N` (lowered)  true → `idx <= N`     → bound is `N + 1`.
///   * `idx < N`             false → `idx >= N`    → no upper bound.
///   * `idx <= N` (lowered)  false → `idx > N`     → no upper bound.
///
/// `IntCmpOp::LessEqual` and `SlessEqual` are not primitives in this
/// IR — pcode-lift lowers them at lift time to `BitNot(Less(N, idx))` at `I1`
/// (resp. `Sless`).  This walker therefore tries two shapes:
///   1. `BitNot(IntLess(IntConst(N), idx))` → bound is `N + 1`.
///   2. `IntCmp(idx, IntConst(N))` with strict-less op → bound is `N`.
///
/// We only return Some on the true-side variants; the false side
/// gives a *lower* bound which doesn't help here.  An over-cautious
/// None is sound; the orchestrator may try again with a stronger
/// classifier next iteration.
///
/// NOTE — `IntCmpOp::Equal` is deliberately NOT handled here.  The
/// taken-true arm of `idx == N` constrains `idx` to the single
/// value `{N}`, NOT `[0, N]`.  The `0..bound` enumeration shape
/// this function feeds into would over-read entries `0..N-1` that
/// `idx == N` never selects, or — if the table has exactly N
/// entries indexed `0..N-1` — read past the table end and fail
/// resolution.  Falling through to the catch-all `None` surfaces
/// the case as `UnresolvedIndirectBranch` instead of mis-resolving.
fn bound_from_if_condition(
    ctx: crate::RewriteCtxView<'_>,
    cond_value: ValueId,
    idx_value: ValueId,
    on_true_branch: bool,
    known: &crate::KnownBitsMap,
) -> Option<u64> {
    if !on_true_branch {
        return None;
    }
    use strider_pattern::{
        Capture, CaptureExt, MatchPat, any_int_const, bool_not, int_cmp_any, var,
    };
    let graph = ctx.graph_ref();
    let cmp_node = graph.producer(cond_value);

    // Shape 1 (lowered <=): BitNot(IntLess(IntConst(N), idx))  or its
    // Sless analogue.  The original `IntLessEqual a, b` opcode lifts
    // with operand-swap to `BitNot(Less(b, a))`; here `a` is `idx`
    // and `b` is `IntConst(N)`, so after swap the `IntConst(N)` is on
    // the LHS of the inner Less.  `int_cmp_any` is non-commutative for
    // Less/Sless, so this orientation is the only one that matches.
    {
        let op_var = Capture::new();
        let n_var = Capture::new();
        let idx_var = Capture::new();
        let pat =
            bool_not(int_cmp_any(any_int_const().capture(n_var), var(idx_var)).capture(op_var))
                .into_pattern();
        if let Some(m) = ctx.matcher().match_at(cmp_node, &pat).expect("classifier pattern is single-rooted") {
            let inner = m.value(idx_var)?;
            if same_value(graph, inner, idx_value) {
                let op = m.bindings().get_int_cmp_op(op_var, ctx.graph_ref())?;
                let accept = match op {
                    IntCmpOp::Less => true,
                    // Signed-less needs a known-non-negative idx; otherwise
                    // the implicit lower bound is INT_MIN and we'd accept
                    // negative runtime values as in-range.  Falling through
                    // to None is the sound choice — the orchestrator
                    // surfaces UnresolvedIndirectBranch at fixed point.
                    IntCmpOp::Sless => is_sign_bit_known_zero(ctx, idx_value, known),
                    _ => false,
                };
                if accept {
                    let n = u64::try_from(m.bindings().get_uint(n_var, ctx.graph_ref())?).ok()?;
                    return n.checked_add(1);
                }
            }
        }
    }

    // Shape 2 (strict <):  IntCmp(idx, IntConst(N))  with Less/Sless.
    let op_var = Capture::new();
    let idx_var = Capture::new();
    let n_var = Capture::new();
    let pat = int_cmp_any(var(idx_var), any_int_const().capture(n_var))
        .capture(op_var)
        .into_pattern();
    let m = ctx.matcher().match_at(cmp_node, &pat).expect("classifier pattern is single-rooted")?;

    // The pattern accepts any LHS; we still verify it refers to the
    // dispatch's `idx_value`.  `same_value` walks through trivial
    // single-input phis, which patterns can't express directly:
    // intermediate orchestrator iterations omit PhiCollapse, so
    // the dispatch region's read of `idx` is wrapped in a
    // single-input `Phi` distinct from the `If`'s direct read.
    let lhs = m.value(idx_var)?;
    if !same_value(graph, lhs, idx_value) {
        return None;
    }
    let n = u64::try_from(m.bindings().get_uint(n_var, ctx.graph_ref())?).ok()?;
    let op = m.bindings().get_int_cmp_op(op_var, ctx.graph_ref())?;

    match op {
        // idx < N (true) → bound = N.
        IntCmpOp::Less => Some(n),
        // Signed-less: see Shape-1 arm for the rationale.  Requires
        // known-non-negative idx, else fall through to Unresolved.
        IntCmpOp::Sless if is_sign_bit_known_zero(ctx, idx_value, known) => Some(n),
        _ => None,
    }
}

/// Returns `true` when [`KnownBits`](crate::KnownBits) proves the
/// sign (high) bit of `idx_value`'s integer type is `0` at every
/// runtime execution — i.e. `idx >= 0` reinterpreted as signed.
///
/// Used to gate the `IntCmpOp::Sless` arm of
/// [`bound_from_if_condition`]: without this proof, `Sless`'s implicit
/// lower bound is `INT_MIN`, not `0`, and we'd advertise target set
/// `0..N` while a negative runtime `idx` reaches OOB.
///
/// Returns `false` for non-integer outputs and for integer types whose
/// width exceeds the 64-bit `KnownBits` representation — both
/// conservatively force the fall-through-to-Unresolved path, which is
/// the sound direction.
fn is_sign_bit_known_zero(
    ctx: crate::RewriteCtxView<'_>,
    idx_value: ValueId,
    known: &crate::KnownBitsMap,
) -> bool {
    let Some(ty) = ctx.graph_ref().value_kind(idx_value).as_value() else {
        return false;
    };
    if !ty.is_integer() || !ty.fits_u64() {
        return false;
    }
    let bit_width = ty.bit_width();
    if bit_width == 0 || bit_width > 64 {
        return false;
    }
    let sign_bit_mask: u64 = 1u64 << (bit_width - 1);
    // KnownBitsFacts defaults to all-unknown for unrecorded outputs, so the check
    // naturally fails closed when the analyzer hasn't seen `idx_value`.
    let kb = known[idx_value];
    kb.zeros & sign_bit_mask == sign_bit_mask
}

/// Defines value identity for the predecessor-If walk.
///
/// Two `ValueId`s match when:
///   * They refer to the same output (the trivial case).
///   * One is the OUTPUT of a single-input unit `Phi` (tagged or anonymous)
///     whose only value input is the other.  This covers the common
///     pattern where the entry region's `If(idx < N)` reads idx
///     directly while the dispatch region's `Load[..idx*stride..]`
///     reads idx through the dispatch region's entry phi.  Without
///     PhiCollapse (which intermediate orchestrator iterations
///     omit) those two reads have different `ValueId`s even
///     though they're trivially identical values.
///
/// We follow the chain transitively so deeper phi nests collapse
/// the same way.  A visited set protects against cycles (back-edges
/// of unsimplified loops); on cycle, we return false rather than
/// looping — same conservative direction as `walk_control_for_if_bound_iter`.
fn same_value(graph: &Graph, a: ValueId, b: ValueId) -> bool {
    // Bidirectionally chase trivial phis: see if either side reduces
    // to the other.  Cap depth to avoid pathological chains.
    fn root(graph: &Graph, mut value: ValueId) -> ValueId {
        use strider_ir::walk::DenseEntitySet;

        let mut budget = 64usize;
        // DenseEntitySet<ValueId> — bit-vector membership check
        // and insert with no hashing.  Stays consistent with the
        // workspace's other entity-keyed visited sets.
        let mut visited: DenseEntitySet<ValueId> = DenseEntitySet::new();
        while budget > 0 {
            if visited.contains(value) {
                break;
            }
            visited.insert(value);
            let node = graph.producer(value);
            match graph.node_kind(node) {
                NodeKind::Phi => {
                    // Slot 0 is the phi-token; slots 1.. are values.
                    // A trivial phi has exactly one value input.
                    if let Ok([_token, val]) = graph.node_inputs_exact::<2>(node) {
                        value = val;
                        budget -= 1;
                        continue;
                    }
                    return value;
                }
                _ => return value,
            }
        }
        value
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
    endianness: strider_target::Endianness,
) -> Option<Vec<u64>> {
    let mut targets = Vec::with_capacity(count as usize);
    for i in 0..count {
        // Address = base + i*stride.  Use saturating math: a wrap
        // here would mean the table runs past u64::MAX, which is
        // physically impossible on any real arch — return None to
        // be safe.
        let offset = i.checked_mul(stride)?;
        let addr = base.checked_add(offset)?;
        // Jump tables live in the loaded image's read-only data
        // (`.rodata`) or sometimes `.text`; both are addressable
        // through the rom's RAM-only `ReadOnlyMemory` surface
        // regardless of the Load's literal `space` field, because
        // the ElfFileMemReader's `ReadOnlyMemory` impl reads through
        // the address-space-agnostic loaded-segments map.
        // Read the RAW entry bytes (the reader no longer decodes), then
        // decode per the target byte order.  `entry_size <= 8` is
        // guaranteed by `match_jump_table_shape`, so the 8-byte buffer
        // always fits.  A partial/unmapped read fails closed (None).
        let mut bytes = [0u8; 8];
        rom.read(addr, &mut bytes[..entry_size]).ok()?;
        let value = u64::try_from(endianness.read_uint(&bytes[..entry_size])).ok()?;
        targets.push(value);
    }
    Some(targets)
}

#[cfg(test)]
#[path = "jump_table_tests.rs"]
mod jump_table_tests;
