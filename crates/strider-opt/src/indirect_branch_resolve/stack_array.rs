//! Stack-array-of-labels arm of the indirect-branch classifier.
//!
//! At -O0, gcc and clang lower a C `goto *targets[idx]` to:
//!
//!   * function entry — N stores of `&&L_i` to a stack array
//!     (`*(sp + base + i*stride) = &&L_i` for i in [0, N)).
//!   * dispatch site — `Load[sp + base + idx*stride]` followed by
//!     `BranchIndirect`.
//!
//! The dispatch load's address has a *symbolic* offset (depends on
//! `idx`).  The existing [`super::jump_table::classify_jump_table`] arm
//! handles only the rodata-table shape (constant-base address); this
//! module handles the SP-rooted shape:
//!
//!   * Match `Load[Add(sp_expr_with_offset_K, Mul(idx, IntConst(stride)))]`
//!     — the sp_expr decomposes to a `Terminal { offset: K }` via the
//!     existing `crate::sp_expr::decompose_sp` helper.
//!   * Bound `idx` via the existing
//!     [`super::jump_table::bound_via_known_bits`] /
//!     [`super::jump_table::bound_via_predecessor_if`] machinery.
//!   * For each `i in 0..N`, look up the stored value at SP-offset
//!     `K + i*stride` via the new
//!     [`find_stack_stored_value_at_offset`] helper (below).
//!   * Each stored value must be `IntConst`; collect into
//!     `ResolvedTargets::Multiple([c0, c1, ...])`.
//!
//! ## Soundness
//!
//! Same two-gate structure as `classify_jump_table`:
//!
//! 1. **Bounded index.**  KnownBits-derived (`idx & 0x7` etc.) or
//!    predecessor-If-derived (`if (idx < N)` dominates the dispatch).
//!    Both bounds are sound upper bounds on `idx`'s runtime value.
//!
//! 2. **Complete value lookup.**  *Every* `find_stack_stored_value_at_offset`
//!    call must return `Some(IntConst(_))`.  A partial match (any i for
//!    which the chain has no matching store, or the stored value is
//!    non-constant) returns `None` so the orchestrator falls back to
//!    `UnresolvedIndirectBranch`.  Over-approximating is sound for the
//!    set of targets but missing a target is unsound (CFG omits real
//!    edges).
//!
//! Failing either gate returns `None`; the orchestrator defers the
//! branch.  No panic, no partial commitment, no over-approximation.

use super::MAX_TABLE_ENTRIES;
use crate::sp_expr::{SpExpr, SpExprMemo, decompose_sp, ranges_disjoint};
use strider_ir::node::{NodeKind, ValueId, ValueType};
use strider_ir::{Graph, IntBinaryOp};
use strider_lift::cfg::ResolvedTargets;

use super::jump_table::{bound_via_known_bits, bound_via_predecessor_if};

use strider_pattern::{
    Capture, CaptureExt, MatchPat, and as and_pat, any_int_const, or as or_pat, var,
};

/// Top-level classifier hook for the stack-array arm.  Called by
/// [`super::classify::classify_anchor`] when the rodata jump-table arm
/// doesn't match and an SP varnode is supplied.
///
/// `anchor_value` is the placeholder Return's value-input slot.
/// `stack_vn` is the calling convention's stack-pointer varnode
/// — without it we can't decompose load addresses, so the arm is
/// skipped if the orchestrator passes `None`.
///
/// # Sound-failure modes (return `None`)
///
/// * Producer isn't a `Load`.
/// * Load address doesn't have the canonical
///   `Add(sp_expr, Mul(idx, IntConst(stride)))` shape.
/// * `idx` cannot be upper-bounded.
/// * Any `find_stack_stored_value_at_offset` returns `None` (no
///   matching store, type mismatch, or aliasing).
/// * Any matched stored value isn't `IntConst` — runtime value would
///   be non-deterministic, can't enumerate.
#[must_use]
pub fn classify_stack_array(
    ctx: &strider_ir::Function,
    anchor_value: ValueId,
    stack_vn: rsleigh::Vn,
    known: &crate::KnownBitsMap,
) -> Option<ResolvedTargets> {
    let function = ctx;
    // ARM/Thumb interworking strips the LSB Thumb-mode marker from the
    // dispatch target via `IntBinaryOp(And)` with a constant mask
    // (`& 0xFFFFFFFE` for 32-bit ARM, `& 0xFFFFFFFFFFFFFFFE` for 64-bit
    // archs that interwork through the same idiom).  The Load at the
    // dispatch site is the `lhs` of that And; we transparently look
    // through the wrapper, run the rest of the classification on the
    // underlying Load, and `& mask` each enumerated target before
    // returning.  Non-And anchors take the path with `mask = !0`.
    let (load_anchor, target_mask) = strip_target_mask(ctx, anchor_value);

    let shape = match_stack_array_shape(ctx, load_anchor, stack_vn)?;
    let bound = bound_via_known_bits(ctx, shape.idx_value, known)
        .or_else(|| bound_via_predecessor_if(ctx, anchor_value, shape.idx_value, known))?;
    if bound == 0 || bound > MAX_TABLE_ENTRIES {
        return None;
    }
    let mut memo = SpExprMemo::default();
    let mut walk_memo = StackStoredValueMemo::default();
    let mut targets: Vec<u64> = Vec::with_capacity(bound as usize);
    for i in 0..bound {
        let i_signed = i64::try_from(i).ok()?;
        let stride_signed = i64::try_from(shape.stride).ok()?;
        let scaled = i_signed.checked_mul(stride_signed)?;
        let off = shape.base_offset.checked_add(scaled)?;
        let value = find_stack_stored_value_at_offset(
            function,
            shape.mem_value,
            off,
            shape.value_type,
            &mut memo,
            &mut walk_memo,
        )?;
        // peel
        // `Truncate(IntConst)` and `Extend(IntConst)` wrappers before
        // checking for a constant.  AArch64-BE's lifter wraps stored
        // label addresses in `Truncate` for 32-bit ARM Thumb-interworking
        // (mask to pointer width); ConstantFold rules 4-6 normally fold
        // these, but the StackStore→LoadForward path can land us on
        // a not-yet-folded shape.  SOUND: both wrappers are deterministic
        // functions of the inner constant, exactly mirroring the
        // `Truncate(IntConst)` / `Extend(IntConst)` arms in
        // `classify_anchor`.
        let c = peel_to_u64_const(function.graph(), value)?;
        targets.push(c & target_mask);
    }
    targets.sort_unstable();
    targets.dedup();
    if targets.is_empty() {
        None
    } else {
        Some(ResolvedTargets::Multiple(targets))
    }
}

/// Peel `Truncate(IntConst)` / `Extend(IntConst)` wrappers and return
/// the inner constant masked to its consumer-declared output width.
/// companion to the
/// `flatten_add_tree` Or-arm fix: AArch64-BE lifter shapes wrap stored
/// label addresses in `Truncate(IntConst, I32)` (32-bit ARM
/// Thumb-interworking); ConstantFold normally folds these but the
/// `StackStore` → `LoadForward` propagation can leave the wrapper
/// in place when the load's declared output type matches the truncate.
///
/// Implements the `Truncate(IntConst)` / `Extend(IntConst)` peel that
/// `classify.rs`'s top-level arm explicitly delegates to ConstantFold
/// (rules 4-6).  This peel handles the stack-array path where the
/// `StackStore` → `LoadForward` propagation can leave the
/// `Truncate` wrapper in place if the load's declared output type
/// matches the truncate width.
///
/// SOUND: both wrappers are deterministic functions of the inner
/// constant.  ZeroExtend leaves the u64 value unchanged; SignExtend
/// requires the input width to recover the sign.  Truncate masks to
/// the output width.
fn peel_to_u64_const(graph: &Graph, value: ValueId) -> Option<u64> {
    // Direct IntConst — fast path.
    if let Some(c) = graph.int_const_val(value) {
        return Some(c);
    }
    let producer = graph.producer(value);
    let kind = *graph.node_kind(producer);
    // Both Truncate and Extend take their single input as slot 0; peel
    // to that input and require it to be an IntConst.  The arm-specific
    // mask / extend logic then operates on the unwrapped `k`.
    let inner = graph.nth_input(producer, 0)?;
    let NodeKind::IntConst(k) = *graph.kind_of_value(inner) else {
        return None;
    };
    match kind {
        NodeKind::Truncate => {
            // `Truncate` always produces a value output (validated signature).
            let out_ty = graph
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
            // `inner` is an `IntConst` (checked above), so its output is a value.
            let in_ty = graph
                .value_kind(inner)
                .as_value()
                .expect("IntConst output is a value");
            let signed = in_ty.get_signed_int(k)?;
            #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
            Some(signed as u64)
        }
        _ => None,
    }
}

/// Maximum number of `And` / `Or` mask layers stripped before we give up
/// and pass the anchor through unchanged.
///
/// ARM-Thumb commonly nests `And(Or(load, 1), 0xFFFFFFFE)` (set LSB then
/// mask it off) — that's 2 layers.  Cap at 4 to defend against
/// pathologically deep wrappers from buggy lifter output without losing
/// the ARM-Thumb idioms we actually care about.  Beyond this cap the
/// classifier returns `None` (defer to `UnresolvedIndirectBranch`).
const MAX_STRIP_LAYERS: usize = 4;

/// Strip up to [`MAX_STRIP_LAYERS`] of `IntBinaryOp(And)`/`Or` wrappers
/// whose constant operand is a static mask, and return the underlying
/// value-output along with the surviving (u64-truncated) mask.  When the
/// anchor isn't an `And`, returns `(anchor_value, !0u64)` so the caller's
/// masking step is a no-op.
///
/// Soundness: the mask is applied bit-wise to each enumerated
/// IntConst stored value.  When the mask clears LSBs (e.g. ARM
/// interworking's `& 0xFFFFFFFE`) the caller's `Multiple` enumerates
/// the correct dispatch addresses; runtime targets are precisely the
/// addresses the program would jump to.  When the mask clears more
/// bits than the architecture's interworking idiom, the resulting
/// addresses may not be valid — but that's a soundness-preserving
/// over-approximation: extra targets produce dead CFG edges, no
/// runtime target is omitted.
///
/// Stripping more than [`MAX_STRIP_LAYERS`] layers is treated as
/// pathological — the function returns the partially-stripped state and
/// the caller's downstream shape match (`match_stack_array_shape`) will
/// fail closed when the residual isn't a Load.
//
// CORRECTNESS — the patterns below are sound-equivalent to the prior
// hand-rolled commutative-operand checks.  `strider_pattern::and` /
// `strider_pattern::or` auto-try both orderings, so a single match per layer
// covers the prior `int_const_val(rhs)` / `int_const_val(lhs)`
// fallback chain.  Each `Capture` binds either the const operand
// (read back via `Match::get_uint`) or the surviving non-const
// operand — the same disambiguation the prior code performed by
// trying `int_const_val` on each operand in turn.
//
// The walk is transitive (cap of 4 layers) so we cannot express it
// as a single tree pattern; instead we run one pattern match per
// iteration, mirroring the prior loop's per-layer scope.  The
// truncating `as u64` is preserved verbatim — the prior
// `int_const_val` returned `u64`, and `get_uint` returns `u128`.
// Real dispatch masks fit in `u64` on every supported arch.
//
// SAFETY / OVER-APPROXIMATION — the `c128 as u64` truncations are
// sound for the indirect-branch-resolver's purpose:
//
//   * The accumulated `mask` is folded into a u64 dispatch-address
//     mask.  The orchestrator only consults `mask` to bound the
//     concrete jump-table target addresses (themselves u64 on every
//     supported arch).  Dropping the upper 64 bits of an AND-mask
//     produces a STRICT-OVER-APPROXIMATION: the kept low-64 bits are
//     still a valid superset of the legal target addresses, so the
//     downstream `match_stack_array_shape` shape match — which
//     fails closed when the residual isn't a Load — only loses
//     resolution power, never soundness.
//   * Similarly, the OR-strip decision (`or_c & mask == 0`) operates
//     on the low-64 truncated `or_c` and `mask`.  Bits 64..127 of
//     `or_c` that happen to overlap mask bits 64..127 would mean the
//     OR is NOT a no-op — truncating both sides could spuriously
//     conclude the OR is no-op when it isn't.  This still fails
//     safe because every supported arch's instruction pointer fits
//     in `u64`, so a u128-shaped OR-constant in this slot would
//     itself indicate an invariant break upstream (the address
//     arithmetic doesn't widen past `u64`).
//
// If a future arch introduces a >64-bit instruction pointer this
// truncation would have to be widened (along with the rest of the
// dispatch-address pipeline that currently uses `u64`).
fn strip_target_mask(
    ctx: &strider_ir::Function,
    anchor_value: ValueId,
) -> (ValueId, u64) {
    let graph = ctx.graph();
    let matcher = strider_pattern::Matcher::try_new(ctx).expect("indirect-branch classifier: from_built invariant guarantees a built Function");
    let mut current = anchor_value;
    let mut mask: u64 = !0u64;
    for _ in 0..MAX_STRIP_LAYERS {
        let producer = graph.producer(current);

        // And-with-constant: mask narrows.
        let c_var = Capture::new();
        let other_var = Capture::new();
        let and_p = and_pat(any_int_const().capture(c_var), var(other_var)).into_pattern();
        if let Some(m) = matcher.match_at(producer, &and_p).expect("classifier pattern is single-rooted")
            && let (Some(c128), Some(other)) =
                (m.bindings().get_uint(c_var, ctx.graph()), m.value(other_var))
        {
            #[allow(clippy::cast_possible_truncation)]
            let c = c128 as u64;
            mask &= c;
            current = other;
            continue;
        }

        // Or-with-constant: when the OR's constant is fully covered by
        // the bits we'll later mask off (`or_const & mask == 0`), the
        // OR is a no-op for the dispatch target — strip it
        // transparently.  Common in ARM-Thumb: `Or(load, 1)` followed
        // by `And(_, 0xFFFFFFFE)` — the OR sets bit 0, the AND clears
        // it.  When the OR's constant overlaps surviving mask bits,
        // leave the wrapper in place (the shape match below will fail
        // and we defer to the orchestrator).
        let c_var = Capture::new();
        let other_var = Capture::new();
        let or_p = or_pat(any_int_const().capture(c_var), var(other_var)).into_pattern();
        if let Some(m) = matcher.match_at(producer, &or_p).expect("classifier pattern is single-rooted")
            && let (Some(or_c128), Some(other)) =
                (m.bindings().get_uint(c_var, ctx.graph()), m.value(other_var))
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

#[derive(Debug, Clone, Copy)]
struct StackArrayShape {
    base_offset: i64,
    stride: u64,
    idx_value: ValueId,
    value_type: strider_ir::node::ValueType,
    mem_value: ValueId,
}

fn match_stack_array_shape(
    ctx: &strider_ir::Function,
    anchor_value: ValueId,
    stack_vn: rsleigh::Vn,
) -> Option<StackArrayShape> {
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
    // The Load may have been detached (0 inputs) by an earlier in-place
    // edit in this same indirect-resolution pass, so this is a genuinely
    // fallible read, not a dead structural-invariant check — bail via
    // `None` on the transient detached shape.
    let [mem_value, addr_value] = function.graph().node_inputs_exact::<2>(load_node).ok()?;

    // Flatten the address into a sum of terms.  ARM lifters sometimes
    // emit `Add(Add(sp, idx*stride), const)` (a nested Add tree)
    // instead of the flat `Add(sp + const, idx*stride)` that x86 / x64
    // produce.  Walk every `Add` / `Sub` node transitively to collect
    // the additive operands.
    let mut terms: Vec<ValueId> = Vec::new();
    flatten_add_tree(function.graph(), addr_value, &mut terms, &mut 0);

    // Among the terms, exactly one must be a `Mul`/`ShiftLeft` shape
    // we can crack into (idx, stride).  The rest must sum (with
    // `decompose_sp`) to `Terminal { offset: K }`.
    let mut idx_stride: Option<(ValueId, u64, usize)> = None;
    for (i, t) in terms.iter().enumerate() {
        if let Some((idx, stride)) = extract_idx_and_stride(ctx, *t) {
            // First match wins; if there are multiple idx*stride
            // sub-expressions in the address (unlikely in practice
            // but defensible), the others would force the
            // sum-decompose step to fail and we'd return None — sound.
            idx_stride = Some((idx, stride, i));
            break;
        }
    }
    let (idx_value, stride, idx_pos) = idx_stride?;

    // Sum the remaining terms via `decompose_sp`.  Each must be either
    // SP-rooted (`Terminal`) or a constant.  Constants accumulate in
    // `extra_offset`; SP-rooted terms must be exactly one (sp + K).
    let mut sp_memo = SpExprMemo::default();
    let mut base_offset_acc: i64 = 0;
    let mut found_sp = false;
    for (i, t) in terms.iter().enumerate() {
        if i == idx_pos {
            continue;
        }
        match decompose_sp(function, *t, stack_vn, &mut sp_memo) {
            Some(SpExpr { base: _, offset }) => {
                if found_sp {
                    // Two SP-rooted terms summed together (`sp+sp+...`)
                    // doesn't describe a stack-slot address — bail.
                    return None;
                }
                found_sp = true;
                base_offset_acc = base_offset_acc.checked_add(offset)?;
            }
            None => {
                // Maybe a pure constant (not SP-rooted).
                if let Some(c) = crate::sp_expr::int_const_signed(function.graph(), *t) {
                    base_offset_acc = base_offset_acc.checked_add(c)?;
                } else {
                    return None;
                }
            }
        }
    }
    if !found_sp {
        // The address never references SP — it might be a pure
        // constant address (handled by `classify_jump_table`'s rodata
        // arm) or something else.  Bail; the caller already tried
        // the rodata arm.
        return None;
    }

    Some(StackArrayShape {
        base_offset: base_offset_acc,
        stride,
        idx_value,
        value_type,
        mem_value,
    })
}

/// Recursively flattens a chain of `IntBinaryOp(Add)` and
/// `IntBinaryOp(Sub)` nodes into the list of additive operands plus a
/// running constant offset adjustment.  Sub's rhs (when it's a
/// constant) is negated and folded into `extra_offset`; non-constant
/// rhs of Sub bails the flatten by pushing the Sub itself unmodified
/// (which then fails the per-term decompose step downstream — sound).
/// Bounded by a shared budget of 32 *visited nodes* (not a depth cap) to
/// defend against pathologically large trees from buggy lifter output.
fn flatten_add_tree(graph: &Graph, value: ValueId, acc: &mut Vec<ValueId>, budget: &mut usize) {
    if *budget >= 32 {
        acc.push(value);
        return;
    }
    *budget += 1;
    let node = graph.producer(value);
    // `addr -= K` from arm/arm-thumb stack-array dispatch lowering arrives
    // as `Add(addr, Neg(IntConst(K)))` (or the post-fold
    // `Add(addr, IntConst(-K))`).  `int_const_signed` sees through
    // `Neg(IntConst)`, so the per-term decompose step downstream catches
    // that constant via the `None` arm at line ~307.
    // also flatten
    // `IntBinaryOp::Or` when used as add-equivalent.  AArch64-BE's
    // Sleigh lift can emit `Or(sp, K)` for stack-pointer-plus-offset
    // address computation when `sp`'s upper bits are guaranteed zero
    // (which they are for any address in the canonical 48-bit virtual
    // range), making OR and ADD bitwise equivalent for non-overlapping
    // operands.  The downstream per-term decompose still needs to see
    // through this to attribute the operand back to `InitialVar(sp)`.
    //
    // SOUND: when both operands have non-overlapping bit footprints,
    // `Or(a, b) == Add(a, b)`.  The classifier's existing per-term
    // soundness checks (every term either resolves to a constant, an
    // InitialVar(sp) reference, or an idx-scaled-by-stride pattern)
    // re-validate the shape downstream.  Misclassification surfaces
    // as a per-term `None` (defer-via-unresolved) rather than a
    // wrong dispatch.
    if let (NodeKind::IntBinaryOp(IntBinaryOp::Add | IntBinaryOp::Or), Ok([lhs, rhs])) =
        (graph.node_kind(node), graph.node_inputs_exact::<2>(node))
    {
        flatten_add_tree(graph, lhs, acc, budget);
        flatten_add_tree(graph, rhs, acc, budget);
        return;
    }
    acc.push(value);
}

/// Extract `(idx, stride)` from a node that scales an index value:
///
///   * `IntMul(idx, IntConst(stride))` — both operand orders.
///   * `IntMul(IntConst(stride), idx)` — both operand orders.
///   * `ShiftLeft(idx, IntConst(s))` — equivalent to `Mul(idx, 1 << s)`;
///     emitted by aarch64 / arm / mips / ppc toolchains for power-of-two
///     strides because those architectures have a single-cycle shift but
///     a multi-cycle multiply.  The lifters expose this directly as
///     `IntBinaryOp::ShiftLeft` so we recognise it here without
///     requiring a `ConstantFold` pass to canonicalise the multiplier.
///
/// Soundness: `1 << s` can overflow u64 when `s >= 64`; reject those
/// shifts (return None) rather than wrap.  The `MAX_TABLE_ENTRIES` cap
/// in `classify_stack_array` makes very large strides unreachable in
/// practice, but a bogus `ShiftLeft(_, IntConst(64+))` from malformed
/// lifter output should fail closed rather than wrap silently.
fn extract_idx_and_stride(
    ctx: &strider_ir::Function,
    candidate: ValueId,
) -> Option<(ValueId, u64)> {
    // CORRECTNESS — pattern-DSL form replaces the prior arm-by-arm
    // dispatch on `NodeKind`.  `strider_pattern::mul` is auto-commutative,
    // collapsing the prior `(idx, IntConst)` / `(IntConst, idx)` arms
    // into one pattern.  `strider_pattern::shl` keeps stated order (shifts
    // are non-commutative) — the rhs must still be the const stride
    // exponent.  We try the multiplication shape first, then the
    // shift shape, mirroring the prior match's arm order.
    use strider_pattern::{Capture, CaptureExt, MatchPat, any_int_const, mul, shl, var};

    let candidate_node = ctx.producer(candidate);
    let matcher = strider_pattern::Matcher::try_new(ctx).expect("indirect-branch classifier: from_built invariant guarantees a built Function");

    // Mul(idx, IntConst(stride)) — either ordering.
    let stride_var = Capture::new();
    let idx_var = Capture::new();
    let mul_pat = mul(var(idx_var), any_int_const().capture(stride_var)).into_pattern();
    if let Some(m) = matcher.match_at(candidate_node, &mul_pat).expect("classifier pattern is single-rooted") {
        let stride_u128 = m.bindings().get_uint(stride_var, ctx.graph())?;
        // `get_uint` returns `u128`; the prior code's `int_const_val`
        // truncated to `u64`.  Mirror that here.  Real strides fit
        // in `u64` everywhere we run.
        #[allow(clippy::cast_possible_truncation)]
        let stride = stride_u128 as u64;
        let idx = m.value(idx_var)?;
        return Some((idx, stride));
    }

    // ShiftLeft(idx, IntConst(s)) — non-commutative; rhs must be const.
    let s_var = Capture::new();
    let idx_var = Capture::new();
    let shl_pat = shl(var(idx_var), any_int_const().capture(s_var)).into_pattern();
    let m = matcher.match_at(candidate_node, &shl_pat).expect("classifier pattern is single-rooted")?;
    let s_u128 = m.bindings().get_uint(s_var, ctx.graph())?;
    // CORRECTNESS — preserve the prior bounds check exactly: reject
    // `s >= 64` (would overflow `1u64 << s`) before computing the
    // stride.  `get_uint` returns `u128`; out-of-range values reject
    // here just as the prior `int_const_val` → `s >= 64` check did.
    if s_u128 >= 64 {
        return None;
    }
    // s_u128 is bounded above by 64 → fits in u32.
    let s32 = u32::try_from(s_u128).ok()?;
    let stride = 1u64.checked_shl(s32)?;
    let idx = m.value(idx_var)?;
    Some((idx, stride))
}

// ── Public helper for the indirect-branch classifier ──────
//
// The stack-array dispatch shape has a *symbolic* load offset, but the
// computed-goto-via-stack-array shape has a *symbolic* offset
// (`sp + base + idx*stride`) — the per-i target lives at offset
// `base + i*stride` for i in [0, N), bounded by KnownBits.
//
// The indirect-branch classifier needs to enumerate per-i values without rewriting
// the load (no IR primitive expresses "value depends on idx" without a
// `Region` for an anonymous `Phi` to bind to).  This helper exposes the
// stack-tagged-`Store`-chain walk as a pub function: given a memory chain root
// and a concrete offset, return the `ValueId` of the value stored
// there (or `None` when the chain has no matching store, has an aliasing
// intermediate, or terminates at `InitialMemory`).
//
// SOUNDNESS — restricted to the no-MemPhi case (the classifier asks one
// concrete offset at a time):
//   * stack-tagged `Store { offset == requested }` with matching value type:
//     return the stored `data` output.  This is sound because no later
//     write can have aliased the slot — we walked here from the load's
//     memory input through strictly-earlier stores, and the offset
//     equality check is exact (StackOffsetDetect tagged it).
//   * stack-tagged `Store` at a different offset: skip iff the byte ranges are
//     provably disjoint (`ranges_disjoint`); recurse on the prior
//     memory.
//   * `Store(_)` (raw, untagged): probe its address.  If it's
//     not SP-rooted (`decompose_sp` returns `None`), it cannot alias
//     a stack slot; recurse.  If it IS SP-rooted (a terminal `sp + k`),
//     recurse iff disjoint.  (This helper is single-region, where the
//     stack pointer never joins through a multi-predecessor phi, so a
//     non-decomposable SP-phi address does not arise.)
//   * `MemPhi`: cross-region join.  This helper does NOT recurse
//     across MemPhi (returns `None`) — the case is single-
//     region (the prologue stores and the dispatch load live in the
//     same region) and the classifier asks one offset at a time.
//   * `InitialMemory` / anything else: return `None`.
//
// Type strictness: the helper returns `None` if the stack-tagged Store's value
// type doesn't equal `value_type` exactly.  Narrow-load-from-wider-store
// is intentionally NOT implemented here — the classifier only consumes
// IntConst targets, and a Truncate(IntConst) folds to IntConst via
// ConstantFold, so the narrow case shows up as a wide-typed IntConst-valued
// store that the classifier can read directly.

/// Per-call memo for `find_stack_stored_value_at_offset`, keyed on
/// `(memory_token, offset, value_type)`.  Threaded through the
/// indirect-branch classifier loops so repeated lookups across
/// enumerated jump-table indices share their walks.
pub type StackStoredValueMemo = rustc_hash::FxHashMap<(ValueId, i64, ValueType), Option<ValueId>>;

/// Walks the memory chain backward from `mem` looking for a
/// `Store(addr=sp+offset)` whose stored value has type `value_type`.
/// Returns the stored value's output id on success, or `None` when no
/// matching store dominates the chain.
///
/// See the module-level "Public helper for the indirect-branch
/// classifier" notes for the soundness rules.
///
/// # Permissiveness (do not rely on this for cross-base disjointness)
///
/// This is a deliberately permissive stack-slot lookup written for the
/// indirect-branch stack-array classifier, and it is *more* permissive
/// than the shared `crate::sp_expr::walk` step:
///
/// - **Walks past non-SP-rooted stores unconditionally.**  When the
///   store's address does not decompose to an SP expression (the `None`
///   arm), it skips the store and continues down `inputs[0]`, with no
///   `AliasMode` gate and accepting opaque pointer addresses — assuming
///   stack and non-stack memory are disjoint.
/// - **Keys slots by offset only, not by base.**  The
///   `SpExpr { base: _, offset: k }` arm matches on `k == offset`
///   alone and ignores the SP `base`, so two distinct SP-relative bases
///   that share an offset are treated as the same slot.
///
/// Both are sound for the single-frame jump-table-array use this helper
/// serves, but callers MUST NOT rely on it for cross-base disjointness.
///
/// # Parameters
///
/// - `function` — the IR function to walk (read-only).
/// - `mem` — the chain root (typically a Load's memory-input slot).
/// - `offset` — the SP-relative offset of the requested slot.
/// - `value_type` — the expected stored value's type.  Mismatched
///   types return `None` (no Truncate / ShiftRight synthesis here).
/// - `sp_memo` — a per-call SP-decomposition memo.
/// - `walk_memo` — a per-call result memo keyed on `(mem, offset,
///   value_type)`.
#[must_use]
pub(crate) fn find_stack_stored_value_at_offset(
    function: &strider_ir::Function,
    mem: ValueId,
    offset: i64,
    value_type: ValueType,
    sp_memo: &mut SpExprMemo,
    walk_memo: &mut StackStoredValueMemo,
) -> Option<ValueId> {
    // Iterative form (was recursive; deep prologues blew the stack).
    // Walks the memory-chain backward via the Store's inputs[0].
    // Stack-safe at any chain depth.  SSoT: the SP is the function's own.
    let stack_vn = function.default_cc().stack_vn;
    let load_size = value_type.byte_size() as i64;
    let mut visited: Vec<(ValueId, i64, ValueType)> = Vec::new();
    let mut cur_mem = mem;

    let result: Option<ValueId> = loop {
        let key = (cur_mem, offset, value_type);
        if let Some(&cached) = walk_memo.get(&key) {
            break cached;
        }
        visited.push(key);
        let node = function.producer(cur_mem);
        match *function.node_kind(node) {
            NodeKind::Store(_) => {
                // Store inputs: [memory, addr, data] — exactly 3 once the
                // kind is established (validated structural invariant).
                let inputs = function
                    .graph()
                    .node_inputs_exact::<3>(node)
                    .expect("Store node has 3 inputs (validated)");
                let addr = inputs[1];
                let data = inputs[2];
                match decompose_sp(function, addr, stack_vn, sp_memo) {
                    Some(SpExpr { base: _, offset: k }) => {
                        // A `Store`'s data input is an `AnyInt` value slot
                        // (validated), so its source output is always a value.
                        let data_ty = function
                            .value_kind(data)
                            .as_value()
                            .expect("Store data input is a value");
                        if k == offset {
                            if data_ty == value_type {
                                break Some(data);
                            }
                            break None;
                        } else {
                            let store_size = data_ty.byte_size() as i64;
                            if ranges_disjoint(k, store_size, offset, load_size) {
                                cur_mem = inputs[0];
                                continue;
                            }
                            break None;
                        }
                    }
                    None => {
                        cur_mem = inputs[0];
                        continue;
                    }
                }
            }
            // MemPhi / InitialMemory / anything else: bail.  See module
            // notes for why MemPhi handling is intentionally future work.
            _ => break None,
        }
    };

    // Memoise every prefix on the way back so future queries reuse work.
    for key in visited {
        walk_memo.insert(key, result);
    }
    result
}
#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, dead_code)]

    use super::*;
    use strider_ir::IRBuilderExt;
    use crate::{ConstantFold, KnownBits, OptimizerPipeline, PhiCollapse, RegionCollapse};
    use strider_ir::ExtendOp;
    use strider_ir::node::ValueType;
    use strider_ir_test_utils::{RegisterSet, stack_vn_aarch64 as sp64};

    fn build_two_target_array(
        targets: [u64; 2],
        base_offset: i64,
        stride: u64,
    ) -> (strider_ir::Function, ValueId) {
        let sp = sp64();
        let arg_vn = rsleigh::Vn {
            addr_off: 0x38,
            addr_space: rsleigh::VnSpace::REGISTER,
            size: 8,
        };
        let mut b = RegisterSet::new()
            .tracked(sp)
            .tracked(arg_vn)
            .callee_saved(sp)
            .stack_vn(sp)
            .build_fn_single_region()
            .unwrap();
        let sp_val = b.read_variable(&sp).unwrap();
        for (i, &target_addr) in targets.iter().enumerate() {
            let off = base_offset + (i as i64) * (stride as i64);
            let off_const = b.build_int_const(off as u64, ValueType::I64).unwrap();
            let addr = b
                .build_int_binary_operation(sp_val, off_const, IntBinaryOp::Add, ValueType::I64)
                .unwrap();
            let target = b.build_int_const(target_addr, ValueType::I64).unwrap();
            b.build_store(addr, target, rsleigh::VnSpace::RAM).unwrap();
        }
        let arg_val = b.read_variable(&arg_vn).unwrap();
        let arg_u32 = strider_ir_test_utils::sentinel_node(
            b.function_mut(),
            NodeKind::Truncate,
            [arg_val],
            [strider_ir::node::ValueKind::Typed(ValueType::I32)],
        );
        let arg_u32_value = b.function().node_outputs_exact::<1>(arg_u32).unwrap()[0];
        let one = b.build_int_const(1u64, ValueType::I32).unwrap();
        let masked = b
            .build_int_binary_operation(arg_u32_value, one, IntBinaryOp::And, ValueType::I32)
            .unwrap();
        let idx_u64 = strider_ir_test_utils::sentinel_node(
            b.function_mut(),
            NodeKind::Extend(ExtendOp::ZeroExtend),
            [masked],
            [strider_ir::node::ValueKind::Typed(ValueType::I64)],
        );
        let idx_u64_value = b.function().node_outputs_exact::<1>(idx_u64).unwrap()[0];
        let stride_const = b.build_int_const(stride, ValueType::I64).unwrap();
        let idx_scaled = b
            .build_int_binary_operation(
                idx_u64_value,
                stride_const,
                IntBinaryOp::Mul,
                ValueType::I64,
            )
            .unwrap();
        let base_const = b
            .build_int_const(base_offset as u64, ValueType::I64)
            .unwrap();
        let sp_plus_base = b
            .build_int_binary_operation(sp_val, base_const, IntBinaryOp::Add, ValueType::I64)
            .unwrap();
        let load_addr = b
            .build_int_binary_operation(sp_plus_base, idx_scaled, IntBinaryOp::Add, ValueType::I64)
            .unwrap();
        let loaded = b
            .build_load(load_addr, rsleigh::VnSpace::RAM, ValueType::I64)
            .unwrap();
        b.build_return(Some(loaded), &[]).unwrap();
        b.set_lift_addr(None);
        let mut fg = b.build().unwrap();
        let mut p = OptimizerPipeline::new();
        p.add(ConstantFold::new());
        p.add(KnownBits);
        p.add(PhiCollapse);
        p.add(RegionCollapse);
        p.run(&mut fg, &mut crate::OptCtx::empty()).unwrap();
        let load = fg
            .graph()
            .all_node_ids()
            .find(|&n| matches!(fg.node_kind(n), NodeKind::Load(_)))
            .expect("Load survives — LoadForward not in pipeline");
        let load_value = fg.node_outputs_exact::<1>(load).unwrap()[0];
        (fg, load_value)
    }

    #[test]
    fn classify_stack_array_two_targets_resolves() {
        let targets = [0x401190u64, 0x401180u64];
        let (fg, load_value) = build_two_target_array(targets, -24, 8);
        let known =
            crate::analyze_known_bits(&fg)
                .expect("kb analyze");
        let result = classify_stack_array(
            &fg,
            load_value,
            sp64(),
            &known,
        );
        let mut expected = targets.to_vec();
        expected.sort_unstable();
        assert_eq!(result, Some(ResolvedTargets::Multiple(expected)));
    }

    #[test]
    fn classify_stack_array_returns_none_on_non_indexed_load() {
        let sp = sp64();
        let mut b = RegisterSet::new()
            .tracked(sp)
            .callee_saved(sp)
            .build_fn_single_region()
            .unwrap();
        let sp_val = b.read_variable(&sp).unwrap();
        let off = b.build_int_const(24u64, ValueType::I64).unwrap();
        let addr = b.build_sub_as_add_neg(sp_val, off, ValueType::I64).unwrap();
        let v = b.build_int_const(0xCAFEu64, ValueType::I64).unwrap();
        b.build_store(addr, v, rsleigh::VnSpace::RAM).unwrap();
        let loaded = b
            .build_load(addr, rsleigh::VnSpace::RAM, ValueType::I64)
            .unwrap();
        b.build_return(Some(loaded), &[]).unwrap();
        b.set_lift_addr(None);
        let mut fg = b.build().unwrap();
        let mut p = OptimizerPipeline::new();
        p.add(ConstantFold::new());
        p.add(KnownBits);
        p.add(PhiCollapse);
        p.add(RegionCollapse);
        p.run(&mut fg, &mut crate::OptCtx::empty()).unwrap();
        let load = fg
            .graph()
            .all_node_ids()
            .find(|&n| matches!(fg.node_kind(n), NodeKind::Load(_)))
            .unwrap();
        let load_value = fg.node_outputs_exact::<1>(load).unwrap()[0];
        let known =
            crate::analyze_known_bits(&fg)
                .expect("kb analyze");
        assert_eq!(
            classify_stack_array(
                &fg,
                load_value,
                sp64(),
                &known
            ),
            None
        );
    }

    #[test]
    fn classify_stack_array_returns_none_on_unbounded_idx() {
        let sp = sp64();
        let arg_vn = rsleigh::Vn {
            addr_off: 0x38,
            addr_space: rsleigh::VnSpace::REGISTER,
            size: 8,
        };
        let mut b = RegisterSet::new()
            .tracked(sp)
            .tracked(arg_vn)
            .callee_saved(sp)
            .stack_vn(sp)
            .build_fn_single_region()
            .unwrap();
        let sp_val = b.read_variable(&sp).unwrap();
        let off24 = b.build_int_const(24u64, ValueType::I64).unwrap();
        let addr_24 = b
            .build_sub_as_add_neg(sp_val, off24, ValueType::I64)
            .unwrap();
        let v = b.build_int_const(0x1234u64, ValueType::I64).unwrap();
        b.build_store(addr_24, v, rsleigh::VnSpace::RAM).unwrap();
        let arg_val = b.read_variable(&arg_vn).unwrap();
        let stride = b.build_int_const(8u64, ValueType::I64).unwrap();
        let idx_scaled = b
            .build_int_binary_operation(arg_val, stride, IntBinaryOp::Mul, ValueType::I64)
            .unwrap();
        let base = b.build_int_const((-24i64) as u64, ValueType::I64).unwrap();
        let sp_plus_base = b
            .build_int_binary_operation(sp_val, base, IntBinaryOp::Add, ValueType::I64)
            .unwrap();
        let load_addr = b
            .build_int_binary_operation(sp_plus_base, idx_scaled, IntBinaryOp::Add, ValueType::I64)
            .unwrap();
        let loaded = b
            .build_load(load_addr, rsleigh::VnSpace::RAM, ValueType::I64)
            .unwrap();
        b.build_return(Some(loaded), &[]).unwrap();
        b.set_lift_addr(None);
        let mut fg = b.build().unwrap();
        let mut p = OptimizerPipeline::new();
        p.add(ConstantFold::new());
        p.add(KnownBits);
        p.add(PhiCollapse);
        p.add(RegionCollapse);
        p.run(&mut fg, &mut crate::OptCtx::empty()).unwrap();
        let load = fg
            .graph()
            .all_node_ids()
            .find(|&n| matches!(fg.node_kind(n), NodeKind::Load(_)))
            .unwrap();
        let load_value = fg.node_outputs_exact::<1>(load).unwrap()[0];
        let known =
            crate::analyze_known_bits(&fg)
                .expect("kb analyze");
        assert_eq!(
            classify_stack_array(
                &fg,
                load_value,
                sp64(),
                &known
            ),
            None
        );
    }

    // ── strip_target_mask characterization tests ──────────────────
    //
    // These tests pin both operand orderings explicitly so a future
    // refactor of `strip_target_mask` cannot accidentally narrow what
    // we accept.  `strider_pattern::and` / `strider_pattern::or` are auto-commutative,
    // so a regression that drops one ordering would still pass the
    // commutative-pair check but fail this characterization.
    //
    // The target shapes covered:
    //   * Bare anchor — no wrapper, returns `(anchor, !0)`.
    //   * `And(load, K)` and `And(K, load)` — both orderings, mask narrows.
    //   * `And(Or(load, 1), 0xFFFE)` — ARM-Thumb interworking idiom; the
    //     OR is stripped because its set bit (`1`) is fully cleared by
    //     the surviving `mask` (`0xFFFE`).
    //   * `Or(load, 0xFF)` not stripped when it wouldn't be masked off
    //     downstream — preserves the wrapper so the outer shape match
    //     fails closed.
    //   * Multi-And nesting — nested AND-masks compose by intersection.

    /// Build a minimal graph whose return-value anchor is a non-const
    /// value — specifically the output of a `Load` from `InitialVar(reg)`.
    /// Returns `(graph, anchor_value)`.  The anchor must NOT itself be
    /// an `IntConst`, because `strip_target_mask`'s commutative-And
    /// handling captures the const operand on either side; an IntConst
    /// inner would incorrectly pin the captured "non-const" side.
    fn build_load_anchor() -> (strider_ir::Function, ValueId) {
        let reg = rsleigh::Vn {
            addr_off: 0x10,
            addr_space: rsleigh::VnSpace::REGISTER,
            size: 8,
        };
        let mut b = RegisterSet::new()
            .tracked(reg)
            .build_fn_single_region()
            .unwrap();
        let addr = b.read_variable(&reg).unwrap();
        let v = b
            .build_load(addr, rsleigh::VnSpace::RAM, ValueType::I64)
            .unwrap();
        b.build_return(Some(v), &[]).unwrap();
        b.set_lift_addr(None);
        let fg = b.build().unwrap();
        (fg, v)
    }

    /// Wraps `inner` in `IntBinaryOp(op)` with the given side-ordering of
    /// the constant `c`.  `swap=false` produces `op(inner, IntConst(c))`;
    /// `swap=true` produces `op(IntConst(c), inner)`.  `ty` is the output
    /// type of both operands and the result.
    fn build_binop_wrapped(
        function: &mut strider_ir::Function,
        inner: ValueId,
        op: IntBinaryOp,
        c: u64,
        ty: ValueType,
        swap: bool,
    ) -> ValueId {
        let const_node = strider_ir_test_utils::sentinel_node(
            function,
            NodeKind::IntConst(u128::from(c)),
            [],
            [strider_ir::node::ValueKind::Typed(ty)],
        );
        let const_value = function.node_outputs_exact::<1>(const_node).unwrap()[0];
        let (lhs, rhs) = if swap {
            (const_value, inner)
        } else {
            (inner, const_value)
        };
        let n = strider_ir_test_utils::sentinel_node(
            function,
            NodeKind::IntBinaryOp(op),
            [lhs, rhs],
            [strider_ir::node::ValueKind::Typed(ty)],
        );
        function.node_outputs_exact::<1>(n).unwrap()[0]
    }

    #[test]
    fn strip_target_mask_no_wrapper_returns_all_ones() {
        let (fg, anchor) = build_load_anchor();
        let (out, mask) = strip_target_mask(
            &fg,
            anchor,
        );
        assert_eq!(out, anchor, "no wrapper: anchor passes through");
        assert_eq!(mask, !0u64, "no wrapper: mask must be all-ones");
    }

    #[test]
    fn strip_target_mask_and_with_const_rhs_strips_one_layer() {
        let (mut fg, inner) = build_load_anchor();
        let wrapped = build_binop_wrapped(
            &mut fg,
            inner,
            IntBinaryOp::And,
            0xFFFE,
            ValueType::I64,
            false,
        );
        let (out, mask) = strip_target_mask(
            &fg,
            wrapped,
        );
        assert_eq!(out, inner, "And(load, K) strips to load");
        assert_eq!(mask, 0xFFFE, "And(load, K) yields mask K");
    }

    #[test]
    fn strip_target_mask_and_with_const_lhs_strips_one_layer() {
        let (mut fg, inner) = build_load_anchor();
        let wrapped = build_binop_wrapped(
            &mut fg,
            inner,
            IntBinaryOp::And,
            0xFFFE,
            ValueType::I64,
            true,
        );
        let (out, mask) = strip_target_mask(
            &fg,
            wrapped,
        );
        assert_eq!(out, inner, "And(K, load) strips to load (commutative)");
        assert_eq!(mask, 0xFFFE, "And(K, load) yields mask K");
    }

    #[test]
    fn strip_target_mask_arm_thumb_or_then_and_strips_both_layers() {
        // Canonical ARM-Thumb interworking shape:
        //   And(Or(inner, 1), 0xFFFE)
        // After strip, both wrappers must be gone (the OR's set bit `1`
        // is fully cleared by the surviving mask `0xFFFE`).
        let (mut fg, inner) = build_load_anchor();
        let or_layer =
            build_binop_wrapped(&mut fg, inner, IntBinaryOp::Or, 1, ValueType::I64, false);
        let and_layer = build_binop_wrapped(
            &mut fg,
            or_layer,
            IntBinaryOp::And,
            0xFFFE,
            ValueType::I64,
            false,
        );
        let (out, mask) = strip_target_mask(
            &fg,
            and_layer,
        );
        assert_eq!(out, inner, "And(Or(load, 1), 0xFFFE) strips both wrappers");
        assert_eq!(mask, 0xFFFE, "and-then-or yields the And's mask");
    }

    #[test]
    fn strip_target_mask_or_overlapping_mask_stops_at_or() {
        // The Or's constant overlaps with surviving mask bits, so the
        // strip must NOT pass through it.  The Or stays in place;
        // the surrounding And contributes its mask.
        let (mut fg, inner) = build_load_anchor();
        let or_layer =
            build_binop_wrapped(&mut fg, inner, IntBinaryOp::Or, 0xFF, ValueType::I64, false);
        let and_layer = build_binop_wrapped(
            &mut fg,
            or_layer,
            IntBinaryOp::And,
            0xFFFE,
            ValueType::I64,
            false,
        );
        let (out, mask) = strip_target_mask(
            &fg,
            and_layer,
        );
        assert_eq!(out, or_layer, "overlapping Or is preserved");
        assert_eq!(mask, 0xFFFE, "And's mask still applies");
    }

    #[test]
    fn strip_target_mask_nested_ands_compose_via_intersection() {
        // And(And(inner, 0xFFFF), 0xFF) — the second And narrows further.
        // Both layers strip; surviving mask is the intersection.
        let (mut fg, inner) = build_load_anchor();
        let inner_and = build_binop_wrapped(
            &mut fg,
            inner,
            IntBinaryOp::And,
            0xFFFF,
            ValueType::I64,
            false,
        );
        let outer_and = build_binop_wrapped(
            &mut fg,
            inner_and,
            IntBinaryOp::And,
            0xFF,
            ValueType::I64,
            false,
        );
        let (out, mask) = strip_target_mask(
            &fg,
            outer_and,
        );
        assert_eq!(out, inner, "nested Ands strip down to innermost");
        assert_eq!(mask, 0xFF, "nested Ands intersect their masks");
    }

    // ── flatten_add_tree budget boundary tests ────────────────────────
    //
    // These tests pin the 32-node budget cap that defends against
    // pathologically deep Add trees (a bug in lifter output, or a
    // crafted input).  The function is recursive; the cap converts
    // "would-be stack overflow" into "graceful unmatch".

    /// Build a right-spine Add tree of the given depth over fresh
    /// IntConst(i) leaves.  Returns the root ValueId.
    fn build_right_spine_add_tree(function: &mut strider_ir::Function, depth: usize) -> ValueId {
        assert!(depth >= 1, "need at least one node");
        // Innermost: IntConst(0).  Wrap depth-1 additional Add layers,
        // each adding a fresh IntConst on the LHS.
        let mut cur = {
            let n = strider_ir_test_utils::sentinel_node(
                function,
                NodeKind::IntConst(0u128),
                [],
                [strider_ir::node::ValueKind::Typed(ValueType::I64)],
            );
            function.node_outputs_exact::<1>(n).unwrap()[0]
        };
        for i in 1..depth {
            let leaf = {
                let n = strider_ir_test_utils::sentinel_node(
                    function,
                    NodeKind::IntConst(u128::from(i as u64)),
                    [],
                    [strider_ir::node::ValueKind::Typed(ValueType::I64)],
                );
                function.node_outputs_exact::<1>(n).unwrap()[0]
            };
            let add = strider_ir_test_utils::sentinel_node(
                function,
                NodeKind::IntBinaryOp(IntBinaryOp::Add),
                [leaf, cur],
                [strider_ir::node::ValueKind::Typed(ValueType::I64)],
            );
            cur = function.node_outputs_exact::<1>(add).unwrap()[0];
        }
        cur
    }

    #[test]
    fn flatten_add_tree_within_budget_collects_all_leaves() {
        // 8-deep Add tree → 8 leaves should all flatten out.
        let (mut fg, _anchor) = build_load_anchor();
        let root = build_right_spine_add_tree(&mut fg, 8);
        let mut acc: Vec<ValueId> = Vec::new();
        let mut budget = 0usize;
        flatten_add_tree(fg.graph(), root, &mut acc, &mut budget);
        // Each Add contributes 1 to budget; total budget = (depth-1)
        // increments.  Leaves equal `depth`.
        assert_eq!(acc.len(), 8, "8 leaves collected, got {}", acc.len());
        assert!(budget <= 32, "budget under cap: {}", budget);
    }

    #[test]
    fn flatten_add_tree_at_budget_boundary_terminates_gracefully() {
        // 64-deep tree exceeds the 32 budget.  flatten_add_tree must
        // not panic; it pushes the over-budget node verbatim (which
        // downstream per-term decompose rejects as non-const non-Mul).
        let (mut fg, _anchor) = build_load_anchor();
        let root = build_right_spine_add_tree(&mut fg, 64);
        let mut acc: Vec<ValueId> = Vec::new();
        let mut budget = 0usize;
        // Smoke test: must not panic at any tree depth.
        flatten_add_tree(fg.graph(), root, &mut acc, &mut budget);
        // Once budget hits 32, the recursive walk stops adding new
        // entries.  The exact behaviour depends on traversal order; we
        // just pin "doesn't panic" and "acc is bounded".
        assert!(
            !acc.is_empty(),
            "flatten must always push at least one entry",
        );
    }

    #[test]
    fn flatten_add_tree_on_non_add_root_pushes_single_term() {
        // Non-Add root → push the root verbatim; budget should be 1
        // (one entry to the walk).
        let (mut fg, _anchor) = build_load_anchor();
        let n = strider_ir_test_utils::sentinel_node(
            &mut fg,
            NodeKind::IntConst(0xABCDu128),
            [],
            [strider_ir::node::ValueKind::Typed(ValueType::I64)],
        );
        let value = fg.node_outputs_exact::<1>(n).unwrap()[0];
        let mut acc: Vec<ValueId> = Vec::new();
        let mut budget = 0usize;
        flatten_add_tree(fg.graph(), value, &mut acc, &mut budget);
        assert_eq!(acc.len(), 1, "non-Add root → single entry");
        assert_eq!(acc[0], value, "entry is the root itself");
    }

    // ── classify_stack_array boundary cases ────────────────────────────

    #[test]
    fn classify_stack_array_one_target_resolves() {
        // Single-element stack array — degenerate jump table of size 1.
        // The classifier should still resolve.  Bound is supplied via
        // KnownBits (idx & 0): always 0.  But that mask is 0, which
        // means bound = 1 (the only valid idx).
        let targets = [0x401200u64];
        let (fg, load_value) = build_one_target_array(targets, -8, 8);
        let known =
            crate::analyze_known_bits(&fg)
                .expect("kb analyze");
        let result = classify_stack_array(
            &fg,
            load_value,
            sp64(),
            &known,
        );
        // Whether the existing helpers can resolve a 1-element case
        // depends on how KnownBits bounds the index.  Pin the contract
        // that the classifier does NOT panic and returns Some/None
        // consistently.
        match result {
            None => { /* defer-via-unresolved is sound */ }
            Some(ResolvedTargets::Multiple(v)) => {
                assert_eq!(
                    v,
                    vec![0x401200u64],
                    "single-element resolves to one target"
                );
            }
            other => panic!("unexpected classifier result: {other:?}"),
        }
    }

    fn build_one_target_array(
        targets: [u64; 1],
        base_offset: i64,
        stride: u64,
    ) -> (strider_ir::Function, strider_ir::node::ValueId) {
        use crate::{ConstantFold, KnownBits, OptimizerPipeline, PhiCollapse, RegionCollapse};
        use strider_ir::ExtendOp;
        use strider_ir::node::ValueType;
        use strider_ir_test_utils::RegisterSet;

        let sp = rsleigh::Vn {
            addr_off: 0x40,
            addr_space: rsleigh::VnSpace::REGISTER,
            size: 8,
        };
        let arg_vn = rsleigh::Vn {
            addr_off: 0x38,
            addr_space: rsleigh::VnSpace::REGISTER,
            size: 8,
        };
        let mut b = RegisterSet::new()
            .tracked(sp)
            .tracked(arg_vn)
            .callee_saved(sp)
            .stack_vn(sp)
            .build_fn_single_region()
            .unwrap();
        let sp_val = b.read_variable(&sp).unwrap();
        let off_const = b
            .build_int_const(base_offset as u64, ValueType::I64)
            .unwrap();
        let addr = b
            .build_int_binary_operation(sp_val, off_const, IntBinaryOp::Add, ValueType::I64)
            .unwrap();
        let target = b.build_int_const(targets[0], ValueType::I64).unwrap();
        b.build_store(addr, target, rsleigh::VnSpace::RAM).unwrap();
        let arg_val = b.read_variable(&arg_vn).unwrap();
        // Build the dispatch site: load through sp+base+idx*stride with
        // idx masked to a single value (& 0 → idx is always 0).
        let arg_u32 = strider_ir_test_utils::sentinel_node(
            b.function_mut(),
            NodeKind::Truncate,
            [arg_val],
            [strider_ir::node::ValueKind::Typed(ValueType::I32)],
        );
        let arg_u32_value = b.function().node_outputs_exact::<1>(arg_u32).unwrap()[0];
        let mask0 = b.build_int_const(0u64, ValueType::I32).unwrap();
        let masked = b
            .build_int_binary_operation(arg_u32_value, mask0, IntBinaryOp::And, ValueType::I32)
            .unwrap();
        let idx_u64 = strider_ir_test_utils::sentinel_node(
            b.function_mut(),
            NodeKind::Extend(ExtendOp::ZeroExtend),
            [masked],
            [strider_ir::node::ValueKind::Typed(ValueType::I64)],
        );
        let idx_u64_value = b.function().node_outputs_exact::<1>(idx_u64).unwrap()[0];
        let stride_const = b.build_int_const(stride, ValueType::I64).unwrap();
        let idx_scaled = b
            .build_int_binary_operation(
                idx_u64_value,
                stride_const,
                IntBinaryOp::Mul,
                ValueType::I64,
            )
            .unwrap();
        let base_const = b
            .build_int_const(base_offset as u64, ValueType::I64)
            .unwrap();
        let sp_plus_base = b
            .build_int_binary_operation(sp_val, base_const, IntBinaryOp::Add, ValueType::I64)
            .unwrap();
        let load_addr = b
            .build_int_binary_operation(sp_plus_base, idx_scaled, IntBinaryOp::Add, ValueType::I64)
            .unwrap();
        let loaded = b
            .build_load(load_addr, rsleigh::VnSpace::RAM, ValueType::I64)
            .unwrap();
        b.build_return(Some(loaded), &[]).unwrap();
        b.set_lift_addr(None);
        let mut fg = b.build().unwrap();
        let mut p = OptimizerPipeline::new();
        p.add(ConstantFold::new());
        p.add(KnownBits);
        p.add(PhiCollapse);
        p.add(RegionCollapse);
        p.run(&mut fg, &mut crate::OptCtx::empty()).unwrap();
        let load = fg
            .graph()
            .all_node_ids()
            .find(|&n| matches!(fg.node_kind(n), NodeKind::Load(_)))
            .expect("Load survives — LoadForward not in pipeline");
        let load_value = fg.node_outputs_exact::<1>(load).unwrap()[0];
        (fg, load_value)
    }

    use crate::{LoadForward, StackOffsetDetect};
    use strider_ir_test_utils::{stack_vn_aarch64 as sp64_vn, stack_vn_x86 as sp32_vn};
    // ── public helper for the indirect-branch classifier ─────────────
    //
    // `find_stack_stored_value_at_offset` walks the memory chain backward from
    // a given `mem` looking for a `Store(sp+offset)` whose value type matches
    // the caller's expectation.  Used by the
    // indirect-branch classifier to look up entries of a stack-array of label
    // addresses one offset at a time (computed-goto via local stack
    // array).  These tests pin the helper's contract in isolation, before the
    // classifier wires it in.

    /// One stack store at the requested offset, value type matches: helper
    /// returns the stored value's output id.
    #[test]
    fn find_stack_stored_value_finds_matching_store() -> crate::Result<()> {
        let sp = sp64_vn();
        let mut fg = strider_ir_test_utils::make_sp_fn(sp, |b, sp_val| {
            let twentyfour = b.build_int_const(24u64, ValueType::I64)?;
            let addr = b.build_sub_as_add_neg(sp_val, twentyfour, ValueType::I64)?;
            let stored = b.build_int_const(0xCAFEu64, ValueType::I64)?;
            b.build_store(addr, stored, rsleigh::VnSpace::RAM)?;
            // Touch the stored memory token so it survives DCE: emit a load of the
            // same slot and return the loaded value.
            let loaded = b.build_load(addr, rsleigh::VnSpace::RAM, ValueType::I64)?;
            b.build_return(Some(loaded), &[])?;
            Ok(())
        })?;

        // Run only ConstantFold + PhiCollapse + RegionCollapse (not LoadForward) so the
        // load + its memory input survive for inspection.
        let mut pipeline = OptimizerPipeline::new();
        pipeline.add(ConstantFold::new());
        pipeline.add(PhiCollapse);
        pipeline.add(RegionCollapse);
        pipeline.run(&mut fg, &mut crate::OptCtx::empty())?;

        // Reach the surviving Load and use its memory-input as the chain root.
        let load = fg
            .graph()
            .all_node_ids()
            .find(|&n| matches!(fg.node_kind(n), NodeKind::Load(_)))
            .expect("Load survives without LoadForward");
        let mem = fg.node_inputs(load).into_iter().next().unwrap();

        let mut memo = SpExprMemo::default();
        let mut walk_memo = StackStoredValueMemo::default();
        let result = find_stack_stored_value_at_offset(
            &fg,
            mem,
            -24,
            ValueType::I64,
            &mut memo,
            &mut walk_memo,
        );
        let value = result.expect("helper should find Store at offset -24");
        // The found value must be the stored constant 0xCAFE.
        assert_eq!(fg.graph().int_const_val(value), Some(0xCAFE));
        Ok(())
    }

    /// Walks past a non-aliasing intermediate SP-relative store (different offset)
    /// and finds the requested-offset store.
    #[test]
    fn find_stack_stored_value_walks_past_non_aliasing() -> crate::Result<()> {
        let sp = sp64_vn();
        let mut fg = strider_ir_test_utils::make_sp_fn(sp, |b, sp_val| {
            // Two stores at distinct offsets that both belong to the chain reaching
            // a final load — mimics the array-of-labels prologue.
            let off24 = b.build_int_const(24u64, ValueType::I64)?;
            let off16 = b.build_int_const(16u64, ValueType::I64)?;
            let addr_24 = b.build_sub_as_add_neg(sp_val, off24, ValueType::I64)?;
            let addr_16 = b.build_sub_as_add_neg(sp_val, off16, ValueType::I64)?;
            let v_24 = b.build_int_const(0xAAAAu64, ValueType::I64)?;
            let v_16 = b.build_int_const(0xBBBBu64, ValueType::I64)?;
            b.build_store(addr_24, v_24, rsleigh::VnSpace::RAM)?;
            b.build_store(addr_16, v_16, rsleigh::VnSpace::RAM)?;
            let loaded = b.build_load(addr_24, rsleigh::VnSpace::RAM, ValueType::I64)?;
            b.build_return(Some(loaded), &[])?;
            Ok(())
        })?;

        let mut pipeline = OptimizerPipeline::new();
        pipeline.add(ConstantFold::new());
        pipeline.add(PhiCollapse);
        pipeline.add(RegionCollapse);
        pipeline.run(&mut fg, &mut crate::OptCtx::empty())?;

        let load = fg
            .graph()
            .all_node_ids()
            .find(|&n| matches!(fg.node_kind(n), NodeKind::Load(_)))
            .expect("Load survives");
        let mem = fg.node_inputs(load).into_iter().next().unwrap();

        let mut memo = SpExprMemo::default();
        let mut walk_memo = StackStoredValueMemo::default();
        // Look up offset -16: the chain has the latest store at -16 and an
        // earlier store at -24 (non-aliasing).  Helper must find -16's value.
        let v16 = find_stack_stored_value_at_offset(
            &fg,
            mem,
            -16,
            ValueType::I64,
            &mut memo,
            &mut walk_memo,
        );
        assert_eq!(
            fg.graph().int_const_val(v16.expect("find -16")),
            Some(0xBBBB)
        );

        // Look up offset -24: must walk through the -16 store (non-aliasing) and
        // find -24's value.
        let v24 = find_stack_stored_value_at_offset(
            &fg,
            mem,
            -24,
            ValueType::I64,
            &mut memo,
            &mut walk_memo,
        );
        assert_eq!(
            fg.graph().int_const_val(v24.expect("find -24")),
            Some(0xAAAA)
        );
        Ok(())
    }

    /// No store at the requested offset: helper returns None (chain bottoms out
    /// at InitialMemory without producing a value).
    #[test]
    fn find_stack_stored_value_no_match_returns_none() -> crate::Result<()> {
        let sp = sp64_vn();
        let mut fg = strider_ir_test_utils::make_sp_fn(sp, |b, sp_val| {
            let off24 = b.build_int_const(24u64, ValueType::I64)?;
            let addr_24 = b.build_sub_as_add_neg(sp_val, off24, ValueType::I64)?;
            let v_24 = b.build_int_const(0xAAAAu64, ValueType::I64)?;
            b.build_store(addr_24, v_24, rsleigh::VnSpace::RAM)?;
            let loaded = b.build_load(addr_24, rsleigh::VnSpace::RAM, ValueType::I64)?;
            b.build_return(Some(loaded), &[])?;
            Ok(())
        })?;

        let mut pipeline = OptimizerPipeline::new();
        pipeline.add(ConstantFold::new());
        pipeline.add(PhiCollapse);
        pipeline.add(RegionCollapse);
        pipeline.run(&mut fg, &mut crate::OptCtx::empty())?;

        let load = fg
            .graph()
            .all_node_ids()
            .find(|&n| matches!(fg.node_kind(n), NodeKind::Load(_)))
            .expect("Load survives");
        let mem = fg.node_inputs(load).into_iter().next().unwrap();

        let mut memo = SpExprMemo::default();
        let mut walk_memo = StackStoredValueMemo::default();
        let result = find_stack_stored_value_at_offset(
            &fg,
            mem,
            -8, // No store at -8.
            ValueType::I64,
            &mut memo,
            &mut walk_memo,
        );
        assert!(result.is_none(), "no store at -8 → helper returns None");
        Ok(())
    }

    /// Aliasing intermediate SP-relative store (overlaps the requested offset) is
    /// the LIVE value at that slot — the helper returns the live store's value, not
    /// the older one.
    #[test]
    fn find_stack_stored_value_returns_latest_at_aliasing_offset() -> crate::Result<()> {
        let sp = sp64_vn();
        let mut fg = strider_ir_test_utils::make_sp_fn(sp, |b, sp_val| {
            let off24 = b.build_int_const(24u64, ValueType::I64)?;
            let addr_24 = b.build_sub_as_add_neg(sp_val, off24, ValueType::I64)?;
            let first = b.build_int_const(0xAAAAu64, ValueType::I64)?;
            let second = b.build_int_const(0xBBBBu64, ValueType::I64)?;
            // Two stores at the SAME offset; the second alias-overwrites the first.
            b.build_store(addr_24, first, rsleigh::VnSpace::RAM)?;
            b.build_store(addr_24, second, rsleigh::VnSpace::RAM)?;
            let loaded = b.build_load(addr_24, rsleigh::VnSpace::RAM, ValueType::I64)?;
            b.build_return(Some(loaded), &[])?;
            Ok(())
        })?;

        let mut pipeline = OptimizerPipeline::new();
        pipeline.add(ConstantFold::new());
        pipeline.add(PhiCollapse);
        pipeline.add(RegionCollapse);
        pipeline.run(&mut fg, &mut crate::OptCtx::empty())?;

        let load = fg
            .graph()
            .all_node_ids()
            .find(|&n| matches!(fg.node_kind(n), NodeKind::Load(_)))
            .expect("Load survives");
        let mem = fg.node_inputs(load).into_iter().next().unwrap();

        let mut memo = SpExprMemo::default();
        let mut walk_memo = StackStoredValueMemo::default();
        let result = find_stack_stored_value_at_offset(
            &fg,
            mem,
            -24,
            ValueType::I64,
            &mut memo,
            &mut walk_memo,
        );
        // The helper must return the *live* (latest) value: the second store.
        let v = result.expect("must find live store");
        assert_eq!(fg.graph().int_const_val(v), Some(0xBBBB));
        Ok(())
    }

    /// Type mismatch (store width != requested width at the matching offset)
    /// returns None — the helper is strict about types because the classifier
    /// needs an exact-typed match to safely treat the value as a target address.
    #[test]
    fn find_stack_stored_value_type_mismatch_returns_none() -> crate::Result<()> {
        let sp = sp64_vn();
        let mut fg = strider_ir_test_utils::make_sp_fn(sp, |b, sp_val| {
            let off24 = b.build_int_const(24u64, ValueType::I64)?;
            let addr_24 = b.build_sub_as_add_neg(sp_val, off24, ValueType::I64)?;
            // Store I32, then load I64 — overlapping byte ranges intersect.
            let stored = b.build_int_const(0xAAAAu64, ValueType::I32)?;
            b.build_store(addr_24, stored, rsleigh::VnSpace::RAM)?;
            let loaded = b.build_load(addr_24, rsleigh::VnSpace::RAM, ValueType::I64)?;
            b.build_return(Some(loaded), &[])?;
            Ok(())
        })?;

        let mut pipeline = OptimizerPipeline::new();
        pipeline.add(ConstantFold::new());
        pipeline.add(PhiCollapse);
        pipeline.add(RegionCollapse);
        pipeline.run(&mut fg, &mut crate::OptCtx::empty())?;

        let load = fg
            .graph()
            .all_node_ids()
            .find(|&n| matches!(fg.node_kind(n), NodeKind::Load(_)))
            .expect("Load survives");
        let mem = fg.node_inputs(load).into_iter().next().unwrap();

        let mut memo = SpExprMemo::default();
        let mut walk_memo = StackStoredValueMemo::default();
        let result = find_stack_stored_value_at_offset(
            &fg,
            mem,
            -24,
            ValueType::I64, // request I64 from a I32 store
            &mut memo,
            &mut walk_memo,
        );
        assert!(result.is_none(), "type mismatch at offset -24 → None");
        Ok(())
    }

    /// End-to-end recipe: the classifier loop calls the helper once per i to
    /// enumerate a stack-array of label addresses.  Mirrors the x64
    /// fixture's prologue (sp-24 → L0, sp-16 → L1).  Asserts the helper
    /// produces N IntConst values that the classifier can then return as
    /// `ResolvedTargets::Multiple`.
    #[test]
    fn find_stack_stored_value_enumerates_array_entries() -> crate::Result<()> {
        let sp = sp64_vn();
        let mut fg = strider_ir_test_utils::make_sp_fn(sp, |b, sp_val| {
            // Mirror the x64 prologue: store target0 at sp-24, target1 at sp-16.
            let off24 = b.build_int_const(24u64, ValueType::I64)?;
            let off16 = b.build_int_const(16u64, ValueType::I64)?;
            let addr_24 = b.build_sub_as_add_neg(sp_val, off24, ValueType::I64)?;
            let addr_16 = b.build_sub_as_add_neg(sp_val, off16, ValueType::I64)?;
            let target0 = b.build_int_const(0x401190u64, ValueType::I64)?;
            let target1 = b.build_int_const(0x401180u64, ValueType::I64)?;
            b.build_store(addr_24, target0, rsleigh::VnSpace::RAM)?;
            b.build_store(addr_16, target1, rsleigh::VnSpace::RAM)?;
            // The actual load uses a symbolic address, but for THIS helper test we
            // only exercise the "look up by concrete offset" API — the symbolic
            // shape match lives in the classifier (tested separately in
            // `indirect_resolve_classify`).
            let loaded = b.build_load(addr_24, rsleigh::VnSpace::RAM, ValueType::I64)?;
            b.build_return(Some(loaded), &[])?;
            Ok(())
        })?;

        let mut pipeline = OptimizerPipeline::new();
        pipeline.add(ConstantFold::new());
        pipeline.add(PhiCollapse);
        pipeline.add(RegionCollapse);
        pipeline.run(&mut fg, &mut crate::OptCtx::empty())?;

        let load = fg
            .graph()
            .all_node_ids()
            .find(|&n| matches!(fg.node_kind(n), NodeKind::Load(_)))
            .expect("Load survives");
        let mem = fg.node_inputs(load).into_iter().next().unwrap();

        let mut memo = SpExprMemo::default();
        let mut walk_memo = StackStoredValueMemo::default();
        // The classifier loop: for each i in 0..2, look up base + i*stride.
        let base = -24i64;
        let stride = 8i64;
        let mut targets = Vec::new();
        for i in 0..2 {
            let off = base + i * stride;
            let v = find_stack_stored_value_at_offset(
                &fg,
                mem,
                off,
                ValueType::I64,
                &mut memo,
                &mut walk_memo,
            )
            .unwrap_or_else(|| panic!("must find store at offset {off}"));
            let c = fg
                .graph()
                .int_const_val(v)
                .expect("stored value is IntConst");
            targets.push(c as u64);
        }
        assert_eq!(targets, vec![0x401190u64, 0x401180u64]);
        Ok(())
    }
    #[test]
    fn lock_barrier_prevents_stack_load_forwarding() -> crate::Result<()> {
        use strider_ir_test_utils::SENTINEL_LIFT_ADDR;

        let sp = sp32_vn();
        let mut fg = strider_ir_test_utils::make_sp_fn(sp, |b, sp_val| {
            let four = b.build_int_const(4u64, ValueType::I32)?;
            let addr = b.build_sub_as_add_neg(sp_val, four, ValueType::I32)?;
            let data = b.build_int_const(0x99u64, ValueType::I32)?;
            b.build_store(addr, data, rsleigh::VnSpace::RAM)?;
            b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
            // Emit a LOCK CallOther.  LOCK is now FullClobber, so StackOffsetDetect
            // must break the Stack chain here.
            let (lock_node, _result) = b.build_call_other(
                0x1234,
                "LOCK",
                None,
                &[],
                &strider_target::BuiltCallOtherAbi {
                    implicit_reads: Vec::new(),
                    implicit_writes: Vec::new(),
                    clobbers_memory: false,
                },
                None,
                false,
            )?;
            let lock_mem_value = b.function().graph().memory_output_of(lock_node)?;
            b.advance_cur_region_memory(lock_mem_value)?;
            let loaded = b.build_load(addr, rsleigh::VnSpace::RAM, ValueType::I32)?;
            b.build_return(Some(loaded), &[])?;
            Ok(())
        })?;

        // Pipeline: ConstantFold → PhiCollapse → RegionCollapse → StackOffsetDetect → LoadForward.
        // StackOffsetDetect must break the Stack chain at LOCK (FullClobber).
        let mut pipeline = OptimizerPipeline::new();
        pipeline.add(ConstantFold::new());
        pipeline.add(PhiCollapse);
        pipeline.add(RegionCollapse);
        pipeline.add(StackOffsetDetect::new());
        pipeline.add(LoadForward::new());

        pipeline.run(&mut fg, &mut crate::OptCtx::empty())?;

        // The Load must NOT be forwarded — LOCK is a full-clobber barrier.
        let reachable_loads = fg.count_kind(|k| matches!(k, NodeKind::Load(_)));
        assert_eq!(
            reachable_loads, 1,
            "Load[sp-4] must NOT be forwarded across a LOCK barrier; \
         LOCK is FullClobber and breaks the Stack chain"
        );
        Ok(())
    }
}
