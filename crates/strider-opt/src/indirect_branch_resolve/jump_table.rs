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
use strider_ir::{Graph, IRViewer};
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
    ctx: &strider_ir::Function,
    anchor_value: ValueId,
    rom: Option<&dyn ReadOnlyMemory>,
    endianness: strider_target::Endianness,
    ranges: &crate::value_range::RangeMap<'_>,
) -> Option<ResolvedTargets> {
    // Structural shape match.  `match_jump_table_shape` returns the
    // `idx` value and the `(base, stride, entry_size)` triple —
    // everything we need to enumerate entries.  Falls through to None
    // for every shape that isn't an honest jump-table dispatch.
    let shape = match_jump_table_shape(ctx, anchor_value)?;

    // Locate the dispatch region: the Region node that owns the IndirectBranch's
    // control edge.  We need it to query the range analysis for the scope
    // where the dispatch executes.
    let dispatch_region = dispatch_region_for_anchor(ctx, anchor_value)?;

    // Bound the index via the dominator-scoped range analysis.  The range pass
    // intersects KnownBits upper bounds with edge-sensitive `If` guards, so it
    // handles both the AND-mask case and the `if (idx < N)` case uniformly.
    let idx_ty = ctx.value_kind(shape.idx_value).as_value()?;
    if !idx_ty.is_integer() {
        return None;
    }
    let idx_mask = idx_ty.bit_mask_u128();
    let bound = ranges
        .range_of(shape.idx_value, dispatch_region)
        .upper_exclusive(idx_mask)?;

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

// ── Dispatch-region locator ──────────────────────────────────────────────────

/// Returns the `Region` node that owns the `IndirectBranch` consuming
/// `anchor_value`.  The IndirectBranch's slot-0 input is a Control value;
/// we walk back through non-Region producer nodes (e.g. `If` outputs) to
/// reach the nearest `Region` — that's the region in which the dispatch
/// executes.
///
/// Returns `None` when no `IndirectBranch` consumes `anchor_value`, or
/// when the control chain doesn't resolve to a `Region`.  Both cases
/// are handled defensively (fail-closed → classifier returns None).
pub(super) fn dispatch_region_for_anchor(
    ctx: &strider_ir::Function,
    anchor_value: ValueId,
) -> Option<NodeId> {
    let graph = ctx.graph();
    // Find the IndirectBranch placeholder that consumes this anchor.
    let placeholder = find_anchor_consumer_placeholder(graph, anchor_value)?;
    // IndirectBranch inputs: [ctrl, mem, target] — slot 0 is control.
    let ctrl = graph
        .node_inputs_exact::<3>(placeholder)
        .expect("IndirectBranch has 3 inputs (validated)")[0];
    // Walk backwards through control-producing nodes that are NOT Regions
    // (e.g. If, Call, CallOther all produce control edges but are not regions).
    let mut node = graph.producer(ctrl);
    loop {
        match graph.node_kind(node) {
            NodeKind::Region | NodeKind::Entry => return Some(node),
            NodeKind::If | NodeKind::Call | NodeKind::CallOther { .. } => {
                // Follow slot-0 control input of the branching node.
                let pred_ctrl = graph.nth_input(node, 0)?;
                node = graph.producer(pred_ctrl);
            }
            _ => return None,
        }
    }
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
/// `IntCmpOp::Less(idx, N)`, so the range analysis currently cannot
/// recover the bound for that pattern.  The shape match still fires,
/// but resolution falls through to `UnresolvedIndirectBranch` until
/// the range pass grows flag-based-cmp support.  Tables with a bound
/// the analyser CAN see (an explicit `idx & MASK` AND-mask, an x86
/// `cmp + jb`/`ja` that lifts to `Less`/`LessEqual` directly, or a
/// signed `cmp + b.lt` on AArch64) resolve normally.
fn match_jump_table_shape(
    ctx: &strider_ir::Function,
    anchor_value: ValueId,
) -> Option<JumpTableShape> {
    let graph = ctx.graph();
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
    if let Some(m) = strider_pattern::Matcher::try_new(ctx)
        .expect("indirect-branch classifier: from_built invariant guarantees a built Function")
        .match_at(load_node, &mul_pat)
        .expect("classifier pattern is single-rooted")
    {
        // `get_uint` returns `Option<u128>`; jump-table bases / strides
        // are addresses + element widths and must fit in u64 on every
        // supported arch.  A wide value here is a malformed match —
        // defer rather than silently routing through a truncated wrong
        // address.
        let base = crate::indirect_branch_resolve::u128_to_branch_target(
            m.bindings().get_uint(base_var, ctx)?,
        )?;
        let stride = crate::indirect_branch_resolve::u128_to_branch_target(
            m.bindings().get_uint(stride_var, ctx)?,
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
    let m = strider_pattern::Matcher::try_new(ctx)
        .expect("indirect-branch classifier: from_built invariant guarantees a built Function")
        .match_at(load_node, &shl_pat)
        .expect("classifier pattern is single-rooted")?;
    let base = crate::indirect_branch_resolve::u128_to_branch_target(
        m.bindings().get_uint(base_var, ctx)?,
    )?;
    let shift = m.bindings().get_uint(stride_var, ctx)?;
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
