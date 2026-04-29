//! Jump-table arm for the tier-2 indirect-branch classifier.
//!
//! Recognises the canonical jump-table dispatch
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

use super::{MAX_TABLE_ENTRIES, ResolvedTargets};
use ir::node::{NodeId, NodeKind, NodeOutputId};
use ir::{BuiltFunctionGraph, Graph, IntCmpOp};
use crate::ReadOnlyMemory;
use rsleigh::VnSpace;

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
    fg: &BuiltFunctionGraph,
    anchor_output: NodeOutputId,
    rom: Option<&dyn ReadOnlyMemory>,
    _link_register_vn: Option<rsleigh::Vn>,
) -> Option<ResolvedTargets> {
    // Step 1: structural shape match.  `match_jump_table_shape`
    // returns the `idx` value and the `(base, stride, entry_size)`
    // triple — everything we need to enumerate entries.  Falls
    // through to None for every shape that isn't an honest
    // jump-table dispatch.
    let shape = match_jump_table_shape(fg, anchor_output)?;

    // Step 2: bound the index.  Two strategies, tried in order:
    //   (a) KnownBits — purely structural inspection of the IR;
    //       cheap; works whenever the shape contains an explicit
    //       AND-mask (`idx & 0x7` etc.).
    //   (b) Predecessor-If walk — looks for an `If(idx < N)` on the
    //       control path leading to the dispatch's region.  Slower
    //       but covers the gcc-emitted "compare-and-branch then
    //       indirect" pattern that has no AND-mask.
    let bound = bound_via_known_bits(fg, shape.idx_output)
        .or_else(|| bound_via_predecessor_if(fg, anchor_output, shape.idx_output))?;

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
    fg: &BuiltFunctionGraph,
    anchor_output: NodeOutputId,
) -> Option<JumpTableShape> {
    let graph = &fg.graph;
    // The producer must be a Load.  classify.rs already routes here
    // only on Load, but we re-check defensively so this function is
    // testable in isolation.  We pull `space` and `entry_size` off the
    // matched node up-front; the pattern-DSL match below then handles
    // the structural shape only.
    let load_node = graph.get_node_from_output(anchor_output);
    let NodeKind::Load(_space) = *graph.node_kind(load_node) else {
        return None;
    };
    // Load output's type tells us the per-entry byte size.  Reject
    // float/bool typed loads — jump tables hold integer pointers.
    let ty = graph.output_kind(anchor_output).as_value()?;
    if !ty.is_integer() {
        return None;
    }
    let entry_size = ty.byte_size();

    // CORRECTNESS — pattern-DSL form is sound-equivalent to the four
    // hand-written commutativity cases the prior version expanded:
    // `pattern::add` and `pattern::mul` are auto-commutative, so the
    // single `load().addr(add(any_int_const(base), mul(var(idx),
    // any_int_const(stride))))` pattern matches all four operand
    // orderings of `(base + idx*stride)` without an explicit fallback
    // chain.  `any_int_const(IntVar)` guarantees the captured side is
    // an `IntConst` node and binds the literal value to the `IntVar`,
    // so on a successful match `idx_output` is necessarily the *other*
    // operand of the multiplication — the same disambiguation the
    // prior `extract_base_and_mul` performed by trying `int_const_val`
    // on each `mul` operand in turn.
    use pattern::{IntVar, Matcher, Var, add, any_int_const, load, mul, var};
    let base_var = IntVar::new();
    let stride_var = IntVar::new();
    let idx_var = Var::new();
    let pat = load().addr(add(
        any_int_const(base_var),
        mul(var(idx_var), any_int_const(stride_var)),
    ));
    let m = Matcher::new(fg).match_at(load_node, &pat.into())?;

    // CORRECTNESS — `IntVar` capture stores the constant value as
    // `u128`; the prior code returned `u64` for both `base` and
    // `stride` via `int_const_val`, which itself truncates to `u64`.
    // We mirror the truncation here.  Real jump-table bases /
    // strides fit in `u64` on every supported arch.
    #[allow(clippy::cast_possible_truncation)]
    let base = m.get_int(base_var)? as u64;
    #[allow(clippy::cast_possible_truncation)]
    let stride = m.get_int(stride_var)? as u64;
    let idx_output = m.get(idx_var)?;

    Some(JumpTableShape {
        base,
        stride,
        idx_output,
        entry_size,
    })
}

// ── Bound via KnownBits ──────────────────────────────────────────────────────

/// Returns an upper bound on `idx_output`'s runtime value, derived from the
/// crate-shared [`opt::analyze_known_bits`](crate::analyze_known_bits)
/// fixed-point analyzer.
///
/// Semantics: if the analyzer proves bit `i` of `idx_output` is always
/// zero, the runtime value cannot have that bit set.  The maximum value is
/// therefore `(!zeros) & type_mask`, and the count of distinct values in
/// `[0, max]` is `max + 1`.  Returns `Some(max + 1)` whenever the analyzer
/// proves at least one upper bit is known zero; otherwise `None` so the
/// caller's predecessor-If fallback gets a chance.
///
/// Replaces a previous local recurrence that re-implemented a stripped-down
/// version of the analyzer's `IntConst` / `And` / `Truncate` /
/// `ZeroExtend` / `ShiftRight` rules.  The fixed-point analyzer covers
/// every node kind those rules covered — and several more (`Or`, `Xor`,
/// `Not`, `Popcount`, `Lzcount`, `ShiftLeft`) — so any bound this function
/// previously returned is still proved, and some previously-unbounded
/// shapes now resolve.
#[must_use]
pub fn bound_via_known_bits(
    fg: &BuiltFunctionGraph,
    idx_output: NodeOutputId,
) -> Option<u64> {
    // Output type: only integer-typed indices make sense as table
    // indices.  Reject everything else (Bool, F32, F64, …).
    let ty = fg.graph.output_kind(idx_output).as_value()?;
    if !ty.is_integer() {
        return None;
    }
    // Type mask sets the maximum possible value (e.g. 0xff for U8).
    // KnownBits at most narrows below this; if no narrowing is
    // possible we return None so the predecessor-If fallback gets a
    // chance.
    let type_mask = u64::try_from(ty.get_unsigned_int(u128::from(u64::MAX))?).ok()?;

    // Outputs absent from `analyze`'s map have no proven bit info; treat
    // them as the all-unknown default.  An analyzer error propagates as
    // None — the caller falls back to the predecessor-If walk and the
    // orchestrator surfaces UnresolvedIndirectBranch at fixed point.
    let known = crate::analyze_known_bits(fg).ok()?;
    let kb = known.get(&idx_output).copied().unwrap_or_default();
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
    fg: &BuiltFunctionGraph,
    anchor_output: NodeOutputId,
    idx_output: NodeOutputId,
) -> Option<u64> {
    // Find the placeholder Return that consumes the anchor.  This
    // is the start of our backward walk.
    //
    // The placeholder Return's input slot 0 is its Control input; we
    // walk upward through Controls looking for an If whose true
    // branch leads to this Return.
    let graph = &fg.graph;
    let return_node = find_anchor_consumer_return(graph, anchor_output)?;
    // Slot 0 = control; see node_signature::expected_signature for
    // Return: `inputs: [CTRL, MEM]; in_tail: RET`.
    let control_in = *graph.node_inputs(return_node).get(0)?;

    let mut visited: HashSet<NodeId> = HashSet::new();
    let mut trail: Vec<NodeId> = Vec::new();
    walk_control_for_if_bound(fg, control_in, idx_output, &mut visited, &mut trail)
}

/// Locates the (single) Return node that consumes `anchor_output` —
/// that's the placeholder `Return(target_value)` the strider lift
/// emits for `UnresolvedIndirectBranch` regions.  Returns None when
/// no consumer is a Return — the producer-shape match should have
/// gated us out before reaching this point, so this is defensive.
fn find_anchor_consumer_return(
    graph: &Graph,
    anchor_output: NodeOutputId,
) -> Option<NodeId> {
    for (consumer_id, _) in graph.output_uses(anchor_output) {
        if matches!(graph.node_kind(consumer_id), NodeKind::Return) {
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
    fg: &BuiltFunctionGraph,
    control_out: NodeOutputId,
    idx_output: NodeOutputId,
    visited: &mut HashSet<NodeId>,
    trail: &mut Vec<NodeId>,
) -> Option<u64> {
    let graph = &fg.graph;
    let producer = graph.get_node_from_output(control_out);
    if !visited.insert(producer) {
        // Cycle (loop back-edge).  Loops can rewrite `idx` per
        // iteration; without loop-level reasoning the bound from
        // the loop entry doesn't hold inside the body.  Fail closed.
        return None;
    }
    trail.push(producer);

    match graph.node_kind(producer) {
        // If's outputs: [true_control, false_control].
        // We're on the path to the dispatch through this If — figure
        // out which branch led to us, then check whether that branch
        // bounds idx.
        NodeKind::If => {
            // Which output of the If is `control_out`?  The output
            // index distinguishes true (0) from false (1) per the
            // node_signature for `If`: `outputs: [CTRL, CTRL]`.
            let (_, output_idx) = graph.output_definition(control_out);
            let if_inputs = graph.node_inputs_exact::<2>(producer).ok()?;
            // If input slot 0 = ctrl predecessor, slot 1 = condition.
            let cond_out = if_inputs[1];
            let on_true = output_idx == 0;
            // Try this If's condition; if it doesn't bound idx, walk
            // up through the If's own control predecessor.  Either
            // failure mode is sound: an If we can't crack open
            // becomes a transparent control-flow node we walk
            // through.
            if let Some(b) = bound_from_if_condition(fg, cond_out, idx_output, on_true) {
                return Some(b);
            }
            walk_control_for_if_bound(fg, if_inputs[0], idx_output, visited, trail)
        }
        // ControlState merges multiple predecessors — every
        // predecessor's path must independently prove the bound, and
        // we take the *max* (the join's effective bound is the
        // weakest of any incoming path).  If any predecessor returns
        // None, the join's bound is None (mixed-bound join → fail
        // closed).
        NodeKind::ControlState => {
            let inputs: Vec<NodeOutputId> =
                graph.node_inputs(producer).into_iter().collect();
            if inputs.is_empty() {
                return None;
            }
            let mut combined: u64 = 0;
            for &pred in &inputs {
                // Save trail length so we can drop the predecessor's
                // visited additions on return — without this rollback,
                // a back-edge in one predecessor's sub-walk would mark
                // the whole join unreachable for every later predecessor.
                let mark = trail.len();
                let bound = walk_control_for_if_bound(fg, pred, idx_output, visited, trail);
                for n in trail.drain(mark..) {
                    visited.remove(&n);
                }
                combined = combined.max(bound?);
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
            let first = graph.node_inputs(producer).into_iter().next()?;
            // Only walk through if the input is a Control output —
            // otherwise we'd derail into data flow.
            if !graph.output_kind(first).is_control() {
                return None;
            }
            walk_control_for_if_bound(fg, first, idx_output, visited, trail)
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
///
/// Shape detection uses the `pattern` crate's `int_cmp_any` builder
/// to capture the comparison's operator and operands in a single
/// match step.  `int_cmp_any` honours each op's commutativity:
/// non-commutative ops (`Less`, `LessEqual`, `Sless`, `SlessEqual`)
/// only bind when `idx` is on the LHS, which is exactly the
/// orientation that proves an upper bound.  The previously
/// hand-rolled "swapped" case checked both orderings and returned
/// None for the swapped one — the pattern simply fails to match
/// there, which is identical at the call site.
///
/// NOTE — `IntCmpOp::Equal` is deliberately NOT handled here.  The
/// taken-true arm of `idx == N` constrains `idx` to the single
/// value `{N}`, NOT `[0, N]`.  The `0..bound` enumeration shape
/// this function feeds into would over-read entries `0..N-1` that
/// `idx == N` never selects, or — if the table has exactly N
/// entries indexed `0..N-1` — read past the table end and fail
/// resolution.  Falling through to the catch-all `None` surfaces
/// the case as `UnresolvedIndirectBranch` instead of mis-resolving.
/// Code-review H2.
fn bound_from_if_condition(
    fg: &BuiltFunctionGraph,
    cond_out: NodeOutputId,
    idx_output: NodeOutputId,
    on_true_branch: bool,
) -> Option<u64> {
    if !on_true_branch {
        return None;
    }
    use pattern::{IntCmpOpVar, IntVar, Matcher, Var, any_int_const, int_cmp_any, var};
    let graph = &fg.graph;
    let cmp_node = graph.get_node_from_output(cond_out);

    let op_var = IntCmpOpVar::new();
    let idx_var = Var::new();
    let n_var = IntVar::new();
    let pat = int_cmp_any(op_var, var(idx_var), any_int_const(n_var));
    let m = Matcher::new(fg).match_at(cmp_node, &pat)?;

    // The pattern accepts any LHS; we still verify it refers to the
    // dispatch's `idx_output`.  `same_value` walks through trivial
    // single-input phis, which patterns can't express directly:
    // intermediate orchestrator iterations omit RedundantPhis, so
    // the dispatch region's read of `idx` is wrapped in a
    // single-input ControlPhi distinct from the `If`'s direct read.
    let lhs = m.get(idx_var)?;
    if !same_value(graph, lhs, idx_output) {
        return None;
    }
    let n = u64::try_from(m.get_int(n_var)?).ok()?;
    let op = m.get_int_cmp_op(op_var)?;

    match op {
        // idx < N (true) → bound = N.
        IntCmpOp::Less | IntCmpOp::Sless => Some(n),
        // idx <= N (true) → bound = N + 1.
        IntCmpOp::LessEqual | IntCmpOp::SlessEqual => n.checked_add(1),
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
fn same_value(graph: &Graph, a: NodeOutputId, b: NodeOutputId) -> bool {
    // Bidirectionally chase trivial phis: see if either side reduces
    // to the other.  Cap depth to avoid pathological chains.
    fn root(graph: &Graph, mut out: NodeOutputId) -> NodeOutputId {
        let mut budget = 64usize;
        let mut visited: HashSet<NodeOutputId> = HashSet::new();
        while budget > 0 && visited.insert(out) {
            let node = graph.get_node_from_output(out);
            match graph.node_kind(node) {
                NodeKind::ControlPhi(_) | NodeKind::ValuePhi => {
                    let inputs: Vec<NodeOutputId> =
                        graph.node_inputs(node).into_iter().collect();
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


#[cfg(test)]
#[path = "jump_table_tests.rs"]
mod jump_table_tests;
