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
use crate::sp_expr::{SpAliasCfg, SpDecomposer, SpExpr, SpExprMemo};
use crate::ReadOnlyMemory;
use crate::AliasMode;
use strider_ir::node::{NodeId, NodeKind, ValueId, ValueType};
use strider_ir::{Function, IRViewer};
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
    branch: NodeId,
    rom: Option<&dyn ReadOnlyMemory>,
    ranges: &crate::value_range::RangeMap<'_>,
    alias_mode: AliasMode,
) -> Option<ResolvedTargets> {
    // The `IndirectBranch` placeholder's slot-2 input ([control, memory,
    // target]) is its current dispatch value — the anchor we analyse.  Taking
    // the branch NODE (not the bare value) means the index-range query below is
    // scoped to the branch ACTUALLY being resolved, never the first
    // `IndirectBranch` that happens to share the dispatch value.
    let [_, _, anchor_value] = ctx.node_inputs_exact::<3>(branch).ok()?;

    // ARM/Thumb interworking strips the LSB Thumb-mode marker from the
    // dispatch target via `IntBinaryOp(And)` with a constant mask
    // (`& 0xFFFFFFFE` for 32-bit ARM).  Look through the wrapper, classify
    // the underlying Load, and `& mask` each enumerated target.  Non-And
    // anchors take `mask = !0` (a no-op).
    let (load_anchor, target_mask) = strip_target_mask(ctx, anchor_value);

    let shape = match_table_shape(ctx, load_anchor)?;

    // Scope the range query to `branch`.  The placeholder is itself a control
    // node in the dominator tree, so a guard whose key dominates it is exactly
    // a guard whose edge dominates the dispatch — and that survives
    // `RegionCollapse` deleting the dispatch Region (the guard then keys on the
    // placeholder directly).
    let idx_ty = ctx.value_kind(shape.idx_value).as_value()?;
    if !idx_ty.is_integer() {
        return None;
    }
    let bound = ranges
        .range_of(shape.idx_value, branch)
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
            // The looked-up slot is a canonical `IntConst` by the time this
            // post-pass runs — `ConstantFold` has folded any
            // `Truncate`/`Extend(IntConst)` wrapper the Store→LoadForward path
            // produced — so read it directly.
            ctx.int_const_val(value)
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
/// The SP-rooted arm decomposes a base term against the function's own
/// `default_cc().stack_vn` (via [`SpDecomposer::new`]); an absolute (rodata)
/// table simply has no SP-rooted term and falls through to a constant base.
fn match_table_shape(ctx: &strider_ir::Function, anchor_value: ValueId) -> Option<TableShape> {
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

    // Crack the address into `idx*stride`, a base node, and any grouped
    // constant `k`.  ConstantFold collapses all constant addends to a single
    // `k` but does NOT canonicalize the variable-Add grouping, so the address
    // is one of three 2-deep shapes (see [`match_dispatch_address`]).
    let (idx_value, stride, base_expr, grouped_k) = match_dispatch_address(function, addr_value)?;

    // Resolve the base to an SP-rooted terminal or an absolute constant,
    // absorbing any constant grouped into it (`add(base, K)` → base + K).
    let mut sp_memo = SpExprMemo::default();
    let mut const_offset = grouped_k;
    let sp_base = match SpDecomposer::new(function, &mut sp_memo).decompose(base_expr) {
        Some(SpExpr { base, offset }) => {
            const_offset = const_offset.checked_add(offset)?;
            Some(base)
        }
        // No SP terminal → the base must be a pure constant (absolute table).
        None => {
            let c = function.int_const_i64(base_expr)?;
            const_offset = const_offset.checked_add(c)?;
            None
        }
    };

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

// ── Dispatch-mask stripping (alignment / mode-tag bits) ──────────────────────

/// Strip the const-masked alignment/mode-tag wrappers that sit ABOVE the table
/// `Load`, returning the underlying value-output and the cumulative
/// (u64-truncated) AND-mask to apply to each enumerated target.
///
/// A compiler only ever wraps an indirect-branch dispatch value in a const
/// `and`/`or` to normalise alignment or a mode tag — never to inject a real
/// fetch-address bit (it would store the correct address in the table instead).
/// So both wrappers collapse to a single concern, "which bits of the loaded
/// entry are artifacts the decode drops", and contribute to one AND-mask:
///
///   * `And(load, mask)` — the explicit alignment mask (ARM `& 0xFFFFFFFE`);
///     applied as-is (`mask &=`).
///   * `Or(load, const)` — alignment/mode tag bits the branch decode drops
///     (the canonical case being the ARM/Thumb `bx (load | 1)` mode bit, but
///     the same holds for any const on any arch — it is never a real address
///     bit).  Cleared (`mask &= !const`).  No `== 1` gate: the soundness comes
///     from the compiler-intent invariant above, not from the bit position.
///
/// The two are matched declaratively as one [`match_at_any`] family and peeled
/// layer-by-layer (handling arbitrarily-nested combinations such as
/// `Or(And(load, m), 1)`) until the producer is no longer a const-masked
/// `and`/`or` — i.e. the `Load` (or some other shape, which the caller's
/// [`match_table_shape`] then fails closed on).  A bare anchor returns
/// `(anchor_value, !0)` (the caller's masking is a no-op).
///
/// The `as u64` truncation is sound — every supported arch's instruction
/// pointer fits in `u64`, so a >64-bit mask constant indicates an upstream
/// invariant break, and a wrong target survives only to be rejected by the CFG
/// builder's out-of-range guard (fail closed).
fn strip_target_mask(ctx: &strider_ir::Function, anchor_value: ValueId) -> (ValueId, u64) {
    use strider_pattern::{
        Capture, CaptureExt, MatchPat, and as and_pat, any_int_const, or as or_pat, var,
    };

    let matcher = strider_pattern::Matcher::try_new(ctx)
        .expect("indirect-branch classifier: from_built invariant guarantees a built Function");

    // One declarative family for the two wrapper shapes.  `inner` is shared
    // (the masked operand, whichever arm wins); the per-op const captures
    // disambiguate which arm matched (`and` → keep the masked bits, `or` →
    // clear the OR-set bits).  Built once, reused across peel iterations.
    let inner = Capture::new();
    let and_c = Capture::new();
    let or_c = Capture::new();
    let pats = [
        and_pat(any_int_const().capture(and_c), var(inner)).into_pattern(),
        or_pat(any_int_const().capture(or_c), var(inner)).into_pattern(),
    ];
    let pat_refs: Vec<_> = pats.iter().collect();

    let mut current = anchor_value;
    let mut mask: u64 = !0u64;
    // Peel one mask layer per iteration.  Each step moves `current` to a
    // strictly deeper operand, so the acyclic producer chain bounds the loop.
    while let Some(m) = matcher
        .match_at_any(ctx.graph().producer(current), &pat_refs)
        .expect("classifier patterns are single-rooted")
    {
        let Some(stripped) = m.value(inner) else { break };
        // `and` keeps its mask bits; `or` clears its set bits (complement so
        // the shared `mask &=` below clears them).  Exactly one capture is
        // bound — the arm that matched.
        let layer = if let Some(c) = m.bindings().get_uint(and_c, ctx) {
            c
        } else if let Some(c) = m.bindings().get_uint(or_c, ctx) {
            !c
        } else {
            break;
        };
        #[allow(clippy::cast_possible_truncation)]
        let layer = layer as u64;
        mask &= layer;
        current = stripped;
    }

    (current, mask)
}

// ── Address shape match (declarative pattern family) ─────────────────────────

/// Cracks a table-dispatch address `base + idx*stride (+ k)` into
/// `(idx, stride, base_node, k)`.  `base_node` is resolved by the caller
/// (SP-rooted terminal or absolute constant); `k` is any constant grouped at
/// the top level (`0` when absent), and the scale is a literal multiply or a
/// power-of-two `ShiftLeft` (aarch64/arm/mips/ppc `LSL`).
///
/// `ConstantFold` collapses constant addends to a single `k` but leaves the
/// variable-`Add` grouping, so the address is one of a small family of shapes,
/// matched **declaratively** against an ordered pattern list (operand order
/// within each `Add` / `Mul` is handled by the matcher's commutativity; the
/// shared captures make the winner's bindings uniform).  Anything outside the
/// family — a deeper tree, a second `idx*stride`, or an `Or` (deliberately not
/// split: `Or(a,b) == Add(a,b)` only for provably-disjoint operands, unproven
/// here, so splitting one would yield a WRONG address and an unsound CFG edge)
/// — fails closed (`None`), leaving the table unresolved.  A new addressing
/// form extends the pattern list, not the control flow.
/// The dispatch-address pattern family plus the shared captures used to read
/// the winner's bindings.  Rebuilt per [`match_dispatch_address`] call: the six
/// shapes are tiny and the call only fires for a branch that reached the
/// table-dispatch arm, so the cost is negligible and we avoid any cross-flow
/// global state (the workspace runs one analysis flow at a time, so a
/// process-wide cache would be unsound to share anyway).
struct DispatchPatterns {
    base: strider_pattern::Capture,
    idx: strider_pattern::Capture,
    stride_mul: strider_pattern::Capture,
    shift_amt: strider_pattern::Capture,
    k: strider_pattern::Capture,
    patterns: Vec<strider_pattern::Pattern>,
}

fn build_dispatch_patterns() -> DispatchPatterns {
    use strider_pattern::{Capture, CaptureExt, MatchPat, add, any_int_const, mul, shl, var};

    let base = Capture::new();
    let idx = Capture::new();
    let stride_mul = Capture::new();
    let shift_amt = Capture::new();
    let k = Capture::new();

    // `idx * stride`, as a multiply or a power-of-two shift.  Built fresh per
    // use (the typed builders are not `Clone`); the shared captures keep the
    // winner's bindings uniform across the family.
    let scaled_mul = || mul(var(idx), any_int_const().capture(stride_mul));
    let scaled_shl = || shl(var(idx), any_int_const().capture(shift_amt));

    // Offset-carrying (more specific) shapes first; `match_at_any` returns the
    // first that fits.
    let patterns = vec![
        add(add(var(base), scaled_mul()), any_int_const().capture(k)).into_pattern(),
        add(add(var(base), scaled_shl()), any_int_const().capture(k)).into_pattern(),
        add(var(base), add(scaled_mul(), any_int_const().capture(k))).into_pattern(),
        add(var(base), add(scaled_shl(), any_int_const().capture(k))).into_pattern(),
        add(var(base), scaled_mul()).into_pattern(),
        add(var(base), scaled_shl()).into_pattern(),
    ];

    DispatchPatterns {
        base,
        idx,
        stride_mul,
        shift_amt,
        k,
        patterns,
    }
}

fn match_dispatch_address(ctx: &Function, addr: ValueId) -> Option<(ValueId, u64, ValueId, i64)> {
    let jt = build_dispatch_patterns();
    let matcher = strider_pattern::Matcher::try_new(ctx)
        .expect("indirect-branch classifier: from_built invariant guarantees a built Function");
    let pat_refs: Vec<_> = jt.patterns.iter().collect();
    let m = matcher
        .match_at_any(ctx.graph().producer(addr), &pat_refs)
        .expect("classifier patterns are single-rooted")?;

    let idx_value = m.value(jt.idx)?;
    let base_expr = m.value(jt.base)?;
    // Stride: the literal multiplier, or `1 << shift` for the shift form
    // (reject `shift >= 64` rather than wrap).
    let stride = match m.bindings().get_uint(jt.stride_mul, ctx) {
        Some(s) => u64::try_from(s).ok()?,
        None => {
            let shift = m.bindings().get_uint(jt.shift_amt, ctx)?;
            1u64.checked_shl(u32::try_from(shift).ok()?)?
        }
    };
    // Offset: the grouped constant `k`, or 0 when the matched shape had none.
    let grouped_k = match m.value(jt.k) {
        Some(k_value) => ctx.int_const_i64(k_value)?,
        None => 0,
    };
    Some((idx_value, stride, base_expr, grouped_k))
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
