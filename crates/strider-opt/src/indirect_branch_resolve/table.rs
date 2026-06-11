//! Unified table-dispatch arm of the indirect-branch classifier.
//!
//! A rodata jump table and an on-stack array of label addresses are the
//! *same* construct — an indexed load of a table of code addresses —
//! differing only in **where the table bytes live**:
//!
//!   * **rodata jump table** — `Load[ base + idx*stride ]` where `base` is
//!     an absolute `IntConst` address.  The entries are bytes in the
//!     binary image, read through a [`ReadOnlyMemory`].
//!   * **on-stack label array** — `Load[ (sp + K) + idx*stride ]` where the
//!     base is SP-rooted.  The entries were written by `Store`s earlier in
//!     the function; we recover them with the memory-SSA walker.
//!
//! So the algorithm factors into a shared skeleton — strip any dispatch
//! mask, flatten the address, pull out the one `idx*stride` term, classify
//! the remaining base as `TableBase::Absolute` or `TableBase::SpRooted`,
//! bound `idx` via the dominator-scoped range analysis, then enumerate
//! `i in 0..N` reading each entry — with a single per-`TableBase` branch
//! for the read.
//!
//! ## Soundness
//!
//! Two independent gates must hold to commit to `Multiple`:
//!
//! 1. **Bounded index.**  The dominator-scoped range analysis bounds `idx`
//!    from an `if (idx < N)` guard dominating the dispatch and/or a
//!    KnownBits mask (`idx & 0x7`).  A sound *upper* bound; mixed-bound
//!    joins fail closed.
//!
//! 2. **Complete table read.**  *Every* entry from `0` through `N-1` must
//!    read back — from the rom (absolute) or as an exact-match,
//!    un-clobbered `Store` (sp-rooted).  Any partial read returns `None`:
//!    a `Multiple` omitting a valid runtime target would wire a CFG
//!    missing real edges.
//!
//! Over-approximating the bound (extra targets) is sound — the surplus
//! become dead CFG edges.  Under-approximating is not.  Failing either
//! gate returns `None` and the orchestrator defers the branch (ultimately
//! `UnresolvedIndirectBranch` at fixed point).  No panic, no partial
//! commitment.

#![allow(clippy::module_name_repetitions)]

use super::MAX_TABLE_ENTRIES;
use crate::sp_expr::{SpAliasCfg, SpDecomposer, SpExpr, SpExprMemo, int_const_signed};
use crate::ReadOnlyMemory;
use crate::AliasMode;
use strider_ir::node::{NodeId, NodeKind, ValueId, ValueType};
use strider_ir::{Function, Graph, IRViewer, IntBinaryOp};
use strider_cfg::ResolvedTargets;

/// Where a dispatch table's bytes live — the single axis on which the
/// rodata and on-stack cases differ.
#[derive(Debug, Clone, Copy)]
enum TableBase {
    /// Absolute address of `table[0]` — read entries from the rom image.
    Absolute(u64),
    /// SP-rooted base: `table[0]` lives at `sp_base + base_offset` — read
    /// entries by walking the memory-SSA chain for the storing `Store`.
    SpRooted {
        /// The SP-derived terminal node output from `decompose_sp` (the
        /// SSoT for the stack frame's base), required by the
        /// `SpAliasCfg` so it rejects stores rooted at a different SP
        /// terminal (e.g. an alignment-masked `sp & mask`).
        sp_base: ValueId,
        /// Signed byte offset of `table[0]` from `sp_base`.
        base_offset: i64,
    },
}

/// Everything the enumeration needs, extracted from a table-dispatch Load.
#[derive(Debug, Clone, Copy)]
struct TableShape {
    /// Where the table lives (drives the per-entry read).
    base: TableBase,
    /// Per-entry stride in bytes (`stride = 1 << shift` for shift-scaled
    /// addressing).
    stride: u64,
    /// The index value the bound must constrain.
    idx_value: ValueId,
    /// The Load's output type — entry width, and the type a recovered
    /// stack store's data must match.
    value_type: ValueType,
    /// Per-entry byte size (`value_type.byte_size()`, `<= 8`).
    entry_size: usize,
    /// The Load's memory-token input — the walk start for the SP-rooted
    /// store lookup.  Unused for the absolute case.
    mem_value: ValueId,
}

/// Top-level classifier hook for the table-dispatch arm.  Called by
/// [`super::classify_anchor`] when the anchor's producer is a
/// [`NodeKind::Load`] or an `IntBinaryOp(And)` dispatch-mask wrapper.
///
/// `anchor_value` is the placeholder `IndirectBranch`'s dispatch-value
/// input.  `rom` is the binary's read-only image (rodata/text); `None`
/// disables the absolute (rodata) arm.  The stack-pointer varnode (for the
/// SP-rooted arm) and the target endianness (for the rodata read) are read
/// off `ctx` — `ctx.default_cc().stack_vn` and `ctx.endianness()`.
#[must_use]
pub fn classify_table_dispatch(
    ctx: &strider_ir::Function,
    anchor_value: ValueId,
    rom: Option<&dyn ReadOnlyMemory>,
    ranges: &crate::value_range::RangeMap<'_>,
    alias_mode: AliasMode,
) -> Option<ResolvedTargets> {
    classify_table_dispatch_scoped(ctx, anchor_value, None, rom, ranges, alias_mode)
}

/// As [`classify_table_dispatch`], but with the resolving branch's placeholder
/// `NodeId` known up front.  The production driver passes `Some(node)` so the
/// index-range query is scoped to the dispatch region of the branch ACTUALLY
/// being resolved — not the first `IndirectBranch` that happens to consume the
/// shared dispatch value (which could carry a tighter, under-approximating
/// guard).  Callers without the node (unit tests) pass `None` and fall back to
/// use-list discovery.
pub(crate) fn classify_table_dispatch_scoped(
    ctx: &strider_ir::Function,
    anchor_value: ValueId,
    placeholder: Option<NodeId>,
    rom: Option<&dyn ReadOnlyMemory>,
    ranges: &crate::value_range::RangeMap<'_>,
    alias_mode: AliasMode,
) -> Option<ResolvedTargets> {
    // ARM/Thumb interworking strips the LSB Thumb-mode marker from the
    // dispatch target via `IntBinaryOp(And)` with a constant mask
    // (`& 0xFFFFFFFE` for 32-bit ARM).  Look through the wrapper, classify
    // the underlying Load, and `& mask` each enumerated target.  Non-And
    // anchors take `mask = !0` (a no-op).
    let (load_anchor, target_mask) = strip_target_mask(ctx, anchor_value);

    // The convention's stack-pointer varnode is always available on the
    // function; the absolute (rodata) shape simply never references it.
    let stack_vn = Some(ctx.default_cc().stack_vn);
    let shape = match_table_shape(ctx, load_anchor, stack_vn)?;

    // Locate the dispatch anchor — the `IndirectBranch` placeholder node —
    // to scope the range query.  Prefer the explicit placeholder (production);
    // fall back to use-list discovery when absent (unit tests).  The
    // placeholder is itself a control node in the dominator tree, so a guard
    // whose key dominates it is exactly a guard whose edge dominates the
    // dispatch — and that survives `RegionCollapse` deleting the dispatch
    // Region (the guard then keys on the placeholder directly).
    let dispatch_anchor = dispatch_anchor_node(ctx, anchor_value, placeholder)?;
    let idx_ty = ctx.value_kind(shape.idx_value).as_value()?;
    if !idx_ty.is_integer() {
        return None;
    }
    let bound = ranges
        .range_of(shape.idx_value, dispatch_anchor)
        .upper_exclusive(idx_ty.bit_mask_u128())?;
    // Enforce the per-call enumeration cap.  Returning None is sound — the
    // orchestrator defers; a later iteration may tighten the bound.
    if bound == 0 || bound > MAX_TABLE_ENTRIES {
        return None;
    }

    let mut sp_memo = SpExprMemo::default();
    let mut targets: Vec<u64> = Vec::with_capacity(bound as usize);
    for i in 0..bound {
        // Fail closed on any partial read — see the module-level soundness
        // note.  A `Multiple` omitting a valid target would wire a CFG
        // missing real edges.
        let entry = read_entry(ctx, &shape, i, rom, &mut sp_memo, alias_mode)?;
        targets.push(entry & target_mask);
    }
    targets.sort_unstable();
    targets.dedup();
    if targets.is_empty() {
        None
    } else {
        Some(ResolvedTargets::Multiple(targets))
    }
}

/// Read the dispatch target for index `i` — from the rom (absolute base)
/// or from the storing `Store` recovered via the memory-SSA walker
/// (sp-rooted base).  `None` on any failed read (fail-closed soundness).
/// The rodata read decodes per the function's own endianness
/// (`ctx.endianness()`, the byte-order SSoT).
fn read_entry(
    ctx: &strider_ir::Function,
    shape: &TableShape,
    i: u64,
    rom: Option<&dyn ReadOnlyMemory>,
    sp_memo: &mut SpExprMemo,
    alias_mode: AliasMode,
) -> Option<u64> {
    match shape.base {
        TableBase::Absolute(base) => {
            // Address = base + i*stride.  Saturating math: a wrap would
            // mean the table runs past u64::MAX — physically impossible —
            // so fail closed.
            let offset = i.checked_mul(shape.stride)?;
            let addr = base.checked_add(offset)?;
            // Jump tables live in the loaded image's `.rodata` / `.text`,
            // addressable through the rom's RAM-only `ReadOnlyMemory`
            // surface regardless of the Load's literal `space`.  Read the
            // RAW entry bytes, then decode per the target byte order.
            // `entry_size <= 8` is guaranteed by `match_table_shape`.
            let rom = rom?;
            let mut bytes = [0u8; 8];
            rom.read(addr, &mut bytes[..shape.entry_size]).ok()?;
            u64::try_from(ctx.endianness().read_uint(&bytes[..shape.entry_size])).ok()
        }
        TableBase::SpRooted {
            sp_base,
            base_offset,
        } => {
            let i_signed = i64::try_from(i).ok()?;
            let stride_signed = i64::try_from(shape.stride).ok()?;
            let off = base_offset.checked_add(i_signed.checked_mul(stride_signed)?)?;
            let value = lookup_stack_slot_via_ssa(
                ctx,
                shape.mem_value,
                sp_base,
                off,
                shape.value_type,
                sp_memo,
                alias_mode,
            )?;
            // Peel `Truncate(IntConst)` / `Extend(IntConst)` wrappers before
            // requiring a constant.  ConstantFold normally folds these, but
            // the Store→LoadForward path can land on a not-yet-folded shape.
            peel_to_u64_const(ctx, value)
        }
    }
}

// ── Shape match ──────────────────────────────────────────────────────────────

/// Recognises the canonical table-dispatch address shape on the producer
/// of `anchor_value` (already mask-stripped by [`strip_target_mask`]).
///
/// The address is flattened into a sum of additive terms; exactly one term
/// must be an `idx*stride` (multiplicative or shift-scaled) sub-expression,
/// and the rest must sum — via `decompose_sp` — to a base that is either:
///
///   * **all-constant** → [`TableBase::Absolute`] (rodata jump table,
///     e.g. `Load[ IntConst(base) + idx*stride ]`), or
///   * **SP-rooted + constant offset** → [`TableBase::SpRooted`] (on-stack
///     label array, e.g. `Load[ (sp + K) + idx*stride ]`).
///
/// `stride` is the literal `IntMul` constant, or `1 << shift` for the
/// `ShiftLeft` (AArch64/ARM `LSL #N`) addressing form.  Both operand
/// orderings of `+` and `*` are handled by the pattern DSL's commutative
/// matching inside [`extract_idx_and_stride`].
///
/// When `stack_vn` is `None` only the absolute arm is reachable (a
/// non-constant base term fails closed).  Every other shape returns
/// `None`.
fn match_table_shape(
    ctx: &strider_ir::Function,
    anchor_value: ValueId,
    stack_vn: Option<rsleigh::Vn>,
) -> Option<TableShape> {
    let function = ctx;
    let load_node = function.producer(anchor_value);
    let NodeKind::Load(_) = *function.node_kind(load_node) else {
        return None;
    };
    // A `Load` always produces a value output (validated signature).
    let value_type = function
        .value_kind(anchor_value)
        .as_value()
        .expect("Load output is a value");
    if !value_type.is_integer() {
        return None;
    }
    let entry_size = value_type.byte_size();
    // A table entry is a machine pointer (<= 8 bytes).  Reject wide loads
    // (I80/I128/...) rather than relying on a downstream size>8 failure.
    if entry_size > 8 {
        return None;
    }
    // The Load may have been detached (0 inputs) by an earlier in-place
    // edit in this pass — a genuinely fallible read, bail via `None`.
    let [mem_value, addr_value] = function.graph().node_inputs_exact::<2>(load_node).ok()?;

    // Flatten the address into a sum of terms.  Handles flat
    // `Add(base, idx*stride)` (x86) and nested `Add(Add(sp, idx*stride), K)`
    // (ARM) trees uniformly.
    let mut terms: Vec<ValueId> = Vec::new();
    flatten_add_tree(function.graph(), addr_value, &mut terms, &mut 0);

    // Exactly one term must crack into (idx, stride).  First match wins; a
    // second `idx*stride` term would force the base sum-decompose to fail
    // (it isn't const / sp-rooted) and we'd return None — sound.
    let mut idx_stride: Option<(ValueId, u64, usize)> = None;
    for (i, t) in terms.iter().enumerate() {
        if let Some((idx, stride)) = extract_idx_and_stride(ctx, *t) {
            idx_stride = Some((idx, stride, i));
            break;
        }
    }
    let (idx_value, stride, idx_pos) = idx_stride?;

    // Sum the remaining terms into the base.  Each is either a pure
    // constant (accumulated as a signed offset) or — when `stack_vn` is
    // supplied — exactly one SP-rooted terminal (`sp + K`).
    let mut sp_memo = SpExprMemo::default();
    let mut const_offset: i64 = 0;
    let mut sp_base: Option<ValueId> = None;
    for (i, t) in terms.iter().enumerate() {
        if i == idx_pos {
            continue;
        }
        // Constant term (sees through `Neg(IntConst)` for `addr - K`).
        if let Some(c) = int_const_signed(function, *t) {
            const_offset = const_offset.checked_add(c)?;
            continue;
        }
        // SP-rooted term — only when the convention's SP varnode is known.
        if stack_vn.is_some()
            && let Some(SpExpr { base, offset }) =
                SpDecomposer::new(function, &mut sp_memo).decompose(*t)
        {
            if sp_base.is_some() {
                // Two SP-rooted terms (`sp + sp + ...`) don't describe a
                // single stack-slot address — bail.
                return None;
            }
            sp_base = Some(base);
            const_offset = const_offset.checked_add(offset)?;
            continue;
        }
        // A term that is neither constant nor an SP-rooted base — not a
        // table dispatch shape we can prove.
        return None;
    }

    let base = match sp_base {
        Some(base) => TableBase::SpRooted {
            sp_base: base,
            base_offset: const_offset,
        },
        // No SP term → an absolute rodata base.  Real rodata addresses are
        // well within `i63`, so a negative (high-bit-set) sum is not a
        // valid base — fail closed rather than wrap.
        None => TableBase::Absolute(u64::try_from(const_offset).ok()?),
    };

    Some(TableShape {
        base,
        stride,
        idx_value,
        value_type,
        entry_size,
        mem_value,
    })
}

// ── Dispatch-region locator ──────────────────────────────────────────────────

/// Returns the `IndirectBranch` placeholder node consuming `anchor_value` —
/// the dispatch site whose dominance the range guard is checked against.
///
/// The placeholder is itself a control node in the dominator tree, so the
/// range query is scoped to it directly (no walk up to an enclosing Region):
/// a guard dominates the dispatch exactly when its key node dominates the
/// placeholder.  This is correct both before `RegionCollapse` (the guard keys
/// on the dispatch Region, which still dominates the placeholder) and after
/// (the guard keys on the placeholder, which dominates itself reflexively).
///
/// Fail-closed (`None`) when no placeholder consumes the anchor.
fn dispatch_anchor_node(
    ctx: &strider_ir::Function,
    anchor_value: ValueId,
    placeholder: Option<NodeId>,
) -> Option<NodeId> {
    match placeholder {
        Some(p) => Some(p),
        None => find_anchor_consumer_placeholder(ctx.graph(), anchor_value),
    }
}

/// Locates the (single) [`NodeKind::IndirectBranch`] that consumes
/// `anchor_value` — the placeholder the strider lift emits for an
/// `UnresolvedIndirectBranch` region.  Defensive `None` when none does.
fn find_anchor_consumer_placeholder(graph: &Graph, anchor_value: ValueId) -> Option<NodeId> {
    crate::first_consumer_of_kind(graph, anchor_value, |k| {
        matches!(k, NodeKind::IndirectBranch)
    })
}

// ── Dispatch-mask stripping (ARM/Thumb interworking) ─────────────────────────

/// Maximum number of `And` / `Or` mask layers stripped before giving up.
/// ARM-Thumb nests `And(Or(load, 1), 0xFFFFFFFE)` — 2 layers; cap at 4 to
/// defend against pathologically deep wrappers without losing the idioms
/// we care about.  Beyond the cap the classifier fails closed.
const MAX_STRIP_LAYERS: usize = 4;

/// Strip up to [`MAX_STRIP_LAYERS`] of `IntBinaryOp(And)`/`Or` wrappers
/// whose constant operand is a static mask, returning the underlying
/// value-output and the surviving (u64-truncated) mask.  A non-`And`/`Or`
/// anchor returns `(anchor_value, !0)` so the caller's masking is a no-op.
///
/// Soundness: the mask is applied bit-wise to each enumerated target.
/// Clearing LSBs (ARM `& 0xFFFFFFFE`) yields the exact dispatch addresses;
/// clearing more bits is a soundness-preserving over-approximation (extra
/// targets → dead CFG edges, no runtime target omitted).  The `as u64`
/// truncations are sound — every supported arch's instruction pointer fits
/// in `u64`, so a >64-bit mask constant here indicates an upstream
/// invariant break and the downstream shape match fails closed.
fn strip_target_mask(ctx: &strider_ir::Function, anchor_value: ValueId) -> (ValueId, u64) {
    use strider_pattern::{Capture, CaptureExt, MatchPat, and as and_pat, any_int_const, or as or_pat, var};

    let graph = ctx.graph();
    let matcher = strider_pattern::Matcher::try_new(ctx)
        .expect("indirect-branch classifier: from_built invariant guarantees a built Function");
    let mut current = anchor_value;
    let mut mask: u64 = !0u64;
    for _ in 0..MAX_STRIP_LAYERS {
        let producer = graph.producer(current);

        // And-with-constant: mask narrows.
        let c_var = Capture::new();
        let other_var = Capture::new();
        let and_p = and_pat(any_int_const().capture(c_var), var(other_var)).into_pattern();
        if let Some(m) = matcher
            .match_at(producer, &and_p)
            .expect("classifier pattern is single-rooted")
            && let (Some(c128), Some(other)) =
                (m.bindings().get_uint(c_var, ctx), m.value(other_var))
        {
            #[allow(clippy::cast_possible_truncation)]
            let c = c128 as u64;
            mask &= c;
            current = other;
            continue;
        }

        // Or-with-constant: when the OR's constant is fully covered by the
        // bits we'll later mask off (`or_const & mask == 0`), the OR is a
        // no-op for the dispatch target — strip it.  Common in ARM-Thumb:
        // `Or(load, 1)` then `And(_, 0xFFFFFFFE)`.  Otherwise leave it (the
        // shape match below fails and we defer).
        let c_var = Capture::new();
        let other_var = Capture::new();
        let or_p = or_pat(any_int_const().capture(c_var), var(other_var)).into_pattern();
        if let Some(m) = matcher
            .match_at(producer, &or_p)
            .expect("classifier pattern is single-rooted")
            && let (Some(or_c128), Some(other)) =
                (m.bindings().get_uint(c_var, ctx), m.value(other_var))
        {
            #[allow(clippy::cast_possible_truncation)]
            let or_c = or_c128 as u64;
            if or_c & mask == 0 {
                current = other;
                continue;
            }
        }

        break;
    }
    (current, mask)
}

// ── Constant peeling (Truncate / Extend wrappers) ────────────────────────────

/// Peel one layer of `Truncate(IntConst)` / `Extend(IntConst)` and return
/// the inner constant, masked / extended to the consumer-declared width.
///
/// `ConstantFold` folds these in the main pipeline, but the
/// `Store` → `LoadForward` propagation can leave the wrapper in place when
/// the load's declared output type matches the truncate width (AArch64-BE
/// lifter shapes wrap stored label addresses in `Truncate(IntConst, I32)`
/// for 32-bit ARM Thumb-interworking).
///
/// SOUND: both wrappers are deterministic functions of the inner constant.
/// ZeroExtend leaves the u64 value unchanged; SignExtend requires the input
/// width to recover the sign; Truncate masks to the output width.
fn peel_to_u64_const(function: &Function, value: ValueId) -> Option<u64> {
    // Direct IntConst — fast path.
    if let Some(c) = function.int_const_val(value) {
        return Some(c);
    }
    let producer = function.producer(value);
    let kind = *function.node_kind(producer);
    let inner = function.graph().nth_input(producer, 0)?;
    let k = function.int_const_u128(inner)?;
    match kind {
        NodeKind::Truncate => {
            let out_ty = function
                .value_kind(value)
                .as_value()
                .expect("Truncate output is a value");
            let masked = k & out_ty.bit_mask_u128();
            #[allow(clippy::cast_possible_truncation)]
            Some(masked as u64)
        }
        NodeKind::Extend(strider_ir::ExtendOp::ZeroExtend) =>
        {
            #[allow(clippy::cast_possible_truncation)]
            Some(k as u64)
        }
        NodeKind::Extend(strider_ir::ExtendOp::SignExtend) => {
            let in_ty = function
                .value_kind(inner)
                .as_value()
                .expect("IntConst output is a value");
            let out_ty = function
                .value_kind(value)
                .as_value()
                .expect("Extend output is a value");
            let signed = in_ty.get_signed_int(k)?;
            // Mask to the Extend's own output width, mirroring the Truncate
            // arm: sign-extension fills bits above the *input* width, but a
            // resolved jump address is only the low `out_ty` bits.  A
            // sub-64-bit SignExtend must not leak sign bits past its declared
            // output into the resolved target (the validator does not pin
            // out_ty == I64 here).
            #[allow(clippy::cast_sign_loss)]
            let masked = (signed as u128) & out_ty.bit_mask_u128();
            #[allow(clippy::cast_possible_truncation)]
            Some(masked as u64)
        }
        _ => None,
    }
}

// ── Address flattening + index/stride extraction ─────────────────────────────

/// Recursively flattens a chain of `IntBinaryOp(Add)` nodes into the list of
/// additive operands.  `Sub`'s constant rhs arrives pre-lowered as
/// `Add(addr, Neg(IntConst(K)))`, caught by the `int_const_signed` term check.
/// Bounded by a shared budget of 32 visited nodes against pathological lifter
/// output.
///
/// `Or` is deliberately NOT flattened.  `Or(a, b) == Add(a, b)` only when the
/// two operands have provably non-overlapping bit footprints; nothing here or
/// downstream proves that, and decompose (`classify_sp_node`) handles only
/// `InitialVar`/`Add`/`And`, so a split `Or` whose operands overlap would
/// yield a WRONG table-entry address and an unsound resolved CFG edge.  We fail
/// closed instead: an `Or` is treated as an opaque term, so a table whose
/// address arithmetic genuinely needs `Or` simply defers (stays unresolved)
/// rather than resolving to a wrong target.  Disjointness-proven `Or`
/// flattening (via the KnownBits lattice: `(!a.zeros) & (!b.zeros) == 0`) could
/// be re-added here with a regression test if a real case requires it.
fn flatten_add_tree(graph: &Graph, value: ValueId, acc: &mut Vec<ValueId>, budget: &mut usize) {
    if *budget >= 32 {
        acc.push(value);
        return;
    }
    *budget += 1;
    let node = graph.producer(value);
    if let (NodeKind::IntBinaryOp(IntBinaryOp::Add), Ok([lhs, rhs])) =
        (graph.node_kind(node), graph.node_inputs_exact::<2>(node))
    {
        flatten_add_tree(graph, lhs, acc, budget);
        flatten_add_tree(graph, rhs, acc, budget);
        return;
    }
    acc.push(value);
}

/// Extract `(idx, stride)` from an index-scaling node:
///
///   * `IntMul(idx, IntConst(stride))` — either operand order.
///   * `ShiftLeft(idx, IntConst(s))` — equivalent to `Mul(idx, 1 << s)`;
///     emitted by aarch64/arm/mips/ppc for power-of-two strides.
///
/// `1 << s` overflows when `s >= 64`; reject those rather than wrap.
fn extract_idx_and_stride(
    ctx: &strider_ir::Function,
    candidate: ValueId,
) -> Option<(ValueId, u64)> {
    use strider_pattern::{Capture, CaptureExt, MatchPat, any_int_const, mul, shl, var};

    let candidate_node = ctx.producer(candidate);
    let matcher = strider_pattern::Matcher::try_new(ctx)
        .expect("indirect-branch classifier: from_built invariant guarantees a built Function");

    // Mul(idx, IntConst(stride)) — auto-commutative.
    let stride_var = Capture::new();
    let idx_var = Capture::new();
    let mul_pat = mul(var(idx_var), any_int_const().capture(stride_var)).into_pattern();
    if let Some(m) = matcher
        .match_at(candidate_node, &mul_pat)
        .expect("classifier pattern is single-rooted")
    {
        let stride_u128 = m.bindings().get_uint(stride_var, ctx)?;
        #[allow(clippy::cast_possible_truncation)]
        let stride = stride_u128 as u64;
        let idx = m.value(idx_var)?;
        return Some((idx, stride));
    }

    // ShiftLeft(idx, IntConst(s)) — non-commutative; rhs must be const.
    let s_var = Capture::new();
    let idx_var = Capture::new();
    let shl_pat = shl(var(idx_var), any_int_const().capture(s_var)).into_pattern();
    let m = matcher
        .match_at(candidate_node, &shl_pat)
        .expect("classifier pattern is single-rooted")?;
    let s_u128 = m.bindings().get_uint(s_var, ctx)?;
    if s_u128 >= 64 {
        return None;
    }
    let s32 = u32::try_from(s_u128).ok()?;
    let stride = 1u64.checked_shl(s32)?;
    let idx = m.value(idx_var)?;
    Some((idx, stride))
}

// ── SP-rooted stack-slot lookup (memory-SSA) ─────────────────────────────────

/// Looks up the value stored at stack slot `[sp_base + offset]` reachable
/// backward from memory token `mem`, via the shared memory-SSA walker
/// (`find_nearest_clobber` + `SpAliasCfg`).
///
/// # Sound-failure modes (return `None`)
///
/// * The nearest clobber from `mem` backward isn't a `Store` (it's a
///   `Call`/`CallOther`/`MemPhi`/`InitialMemory`/opaque) — all genuine
///   clobber boundaries.
/// * The nearest clobber IS a `Store` but not an exact match for
///   `sp_base + offset` (a may-alias at a different slot).
/// * Exact-match `Store` whose stored value's type != `value_type`.
///
/// # Soundness
///
/// The oracle runs under the caller-supplied [`AliasMode`] (threaded from
/// [`OptOptions::alias_mode`](crate::OptOptions)) and `call_clobbers =
/// true`: the label array is in the caller's frame (SP-rooted) and a `Call`
/// between the stores and the dispatch load may expose the frame to the
/// callee, so the table cannot be trusted past a `Call`.  Under
/// [`AliasMode::Strict`] a global (constant-address) store between the
/// prologue stores and the dispatch load may-aliases the SP-rooted probe and
/// surfaces as a clobber (returns `None` → the branch defers); under
/// [`AliasMode::StackGlobalDisjoint`] such a store is proven disjoint and the
/// walk continues to the prologue store.
#[must_use]
fn lookup_stack_slot_via_ssa(
    function: &Function,
    mem: ValueId,
    sp_base: ValueId,
    offset: i64,
    value_type: ValueType,
    sp_memo: &mut SpExprMemo,
    alias_mode: AliasMode,
) -> Option<ValueId> {
    // Probe the table-entry slot via the shared memory-SSA store lookup.
    // The entry's own width (`value_type.byte_size()`) is the probe width, so a
    // partial tail-overlap of the entry surfaces as a clobber (returned
    // non-anchored) rather than being walked past.
    let load_size = value_type.byte_size() as i64;
    // A Call may expose the SP-rooted label array to a callee (`call_clobbers:
    // true`); the jump-table classifier stays conservative on distinct SP bases.
    let store = SpAliasCfg::call_blocking(sp_memo, alias_mode)
        .reaching_store(function, mem, sp_base, offset, load_size)?;
    // Exact match: anchored at the slot AND the stored type equals the
    // requested table-entry type (which pins the width).
    if store.store_offset != offset {
        return None;
    }
    let data_ty = function.value_kind(store.data).as_value()?;
    (data_ty == value_type).then_some(store.data)
}

#[cfg(test)]
#[path = "table_tests.rs"]
mod table_tests;
