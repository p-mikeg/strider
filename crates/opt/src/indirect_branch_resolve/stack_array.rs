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
//!     existing [`opt::sp_expr::decompose_sp`] helper.
//!   * Bound `idx` via the existing
//!     [`super::jump_table::bound_via_known_bits`] /
//!     [`super::jump_table::bound_via_predecessor_if`] machinery.
//!   * For each `i in 0..N`, look up the stored value at SP-offset
//!     `K + i*stride` via the new
//!     [`opt::stack_load_forward::find_stack_stored_value_at_offset`]
//!     helper.
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

use super::{MAX_TABLE_ENTRIES, ResolvedTargets};
use ir::node::{NodeKind, NodeOutputId};
use ir::{BuiltFunctionGraph, Graph, IntBinaryOp};
use crate::sp_expr::{SpExpr, SpExprMemo, decompose_sp};
use crate::stack_load_forward::{StackStoredValueMemo, find_stack_stored_value_at_offset};

use super::jump_table::{bound_via_known_bits, bound_via_predecessor_if};

use pattern::{Capture, Matcher, and as and_pat, any_int_const, or as or_pat, var};

/// Top-level classifier hook for the stack-array arm.  Called by
/// [`super::classify::classify_anchor_with_rom_and_sp`] when the
/// rodata jump-table arm doesn't match and an SP varnode is supplied.
///
/// `anchor_output` is the placeholder Return's value-input slot.
/// `stack_ptr_vn` is the calling convention's stack-pointer varnode
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
    fg: &BuiltFunctionGraph,
    anchor_output: NodeOutputId,
    stack_ptr_vn: rsleigh::Vn,
    known: &rustc_hash::FxHashMap<NodeOutputId, crate::Kb>,
) -> Option<ResolvedTargets> {
    let graph = &fg.graph;
    // ARM/Thumb interworking strips the LSB Thumb-mode marker from the
    // dispatch target via `IntBinaryOp(And)` with a constant mask
    // (`& 0xFFFFFFFE` for 32-bit ARM, `& 0xFFFFFFFFFFFFFFFE` for 64-bit
    // archs that interwork through the same idiom).  The Load at the
    // dispatch site is the `lhs` of that And; we transparently look
    // through the wrapper, run the rest of the classification on the
    // underlying Load, and `& mask` each enumerated target before
    // returning.  Non-And anchors take the path with `mask = !0`.
    let (load_anchor, target_mask) = strip_target_mask(fg, anchor_output);

    let shape = match_stack_array_shape(fg, load_anchor, stack_ptr_vn)?;
    let bound = bound_via_known_bits(fg, shape.idx_output, known)
        .or_else(|| bound_via_predecessor_if(fg, anchor_output, shape.idx_output))?;
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
            graph,
            shape.mem_input,
            off,
            shape.value_type,
            stack_ptr_vn,
            &mut memo,
            &mut walk_memo,
        )?;
        let c = graph.int_const_val(value)?;
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
/// anchor isn't an `And`, returns `(anchor_output, !0u64)` so the caller's
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
// hand-rolled commutative-operand checks.  `pattern::and` /
// `pattern::or` auto-try both orderings, so a single match per layer
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
fn strip_target_mask(
    fg: &BuiltFunctionGraph,
    anchor_output: NodeOutputId,
) -> (NodeOutputId, u64) {
    let graph = &fg.graph;
    let matcher = Matcher::new(fg);
    let mut current = anchor_output;
    let mut mask: u64 = !0u64;
    for _ in 0..MAX_STRIP_LAYERS {
        let producer = graph.get_node_from_output(current);

        // And-with-constant: mask narrows.
        let c_var = Capture::new();
        let other_var = Capture::new();
        let and_p = and_pat(any_int_const(c_var), var(other_var));
        if let Some(m) = matcher.match_at(producer, &and_p.into())
            && let (Some(c128), Some(other)) = (m.get_uint(c_var, fg), m.output(other_var))
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
        let or_p = or_pat(any_int_const(c_var), var(other_var));
        if let Some(m) = matcher.match_at(producer, &or_p.into())
            && let (Some(or_c128), Some(other)) = (m.get_uint(c_var, fg), m.output(other_var))
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
    idx_output: NodeOutputId,
    value_type: ir::node::NodeOutputType,
    mem_input: NodeOutputId,
}

fn match_stack_array_shape(
    fg: &BuiltFunctionGraph,
    anchor_output: NodeOutputId,
    stack_ptr_vn: rsleigh::Vn,
) -> Option<StackArrayShape> {
    let graph = &fg.graph;
    let load_node = graph.get_node_from_output(anchor_output);
    let NodeKind::Load(_) = *graph.node_kind(load_node) else {
        return None;
    };
    let value_type = graph.output_kind(anchor_output).as_value()?;
    if !value_type.is_integer() {
        return None;
    }
    let [mem_input, addr_output] = graph.node_inputs_exact::<2>(load_node).ok()?;

    // Flatten the address into a sum of terms.  ARM lifters sometimes
    // emit `Add(Add(sp, idx*stride), const)` (a nested Add tree)
    // instead of the flat `Add(sp + const, idx*stride)` that x86 / x64
    // produce.  Walk every `Add` / `Sub` node transitively to collect
    // the additive operands.
    let mut terms: Vec<NodeOutputId> = Vec::new();
    flatten_add_tree(graph, addr_output, &mut terms, &mut 0);

    // Among the terms, exactly one must be a `Mul`/`ShiftLeft` shape
    // we can crack into (idx, stride).  The rest must sum (with
    // `decompose_sp`) to `Terminal { offset: K }`.
    let mut idx_stride: Option<(NodeOutputId, u64, usize)> = None;
    for (i, t) in terms.iter().enumerate() {
        if let Some((idx, stride)) = extract_idx_and_stride(fg, *t) {
            // First match wins; if there are multiple idx*stride
            // sub-expressions in the address (unlikely in practice
            // but defensible), the others would force the
            // sum-decompose step to fail and we'd return None — sound.
            idx_stride = Some((idx, stride, i));
            break;
        }
    }
    let (idx_output, stride, idx_pos) = idx_stride?;

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
        let mut visiting = rustc_hash::FxHashSet::default();
        match decompose_sp(graph, *t, stack_ptr_vn, &mut sp_memo, &mut visiting) {
            Some(SpExpr::Terminal { base: _, offset }) => {
                if found_sp {
                    // Two SP-rooted terms summed together (`sp+sp+...`)
                    // doesn't describe a stack-slot address — bail.
                    return None;
                }
                found_sp = true;
                base_offset_acc = base_offset_acc.checked_add(offset)?;
            }
            Some(SpExpr::Phi { .. }) => {
                // SP through a phi-join — out of scope for the
                // single-region shape.  Bail.
                return None;
            }
            None => {
                // Maybe a pure constant (not SP-rooted).
                if let Some(c) = crate::sp_expr::int_const_signed(graph, *t) {
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
        idx_output,
        value_type,
        mem_input,
    })
}

/// Recursively flattens a chain of `IntBinaryOp(Add)` and
/// `IntBinaryOp(Sub)` nodes into the list of additive operands plus a
/// running constant offset adjustment.  Sub's rhs (when it's a
/// constant) is negated and folded into `extra_offset`; non-constant
/// rhs of Sub bails the flatten by pushing the Sub itself unmodified
/// (which then fails the per-term decompose step downstream — sound).
/// Capped at 32 nodes to defend against pathologically deep trees from
/// buggy lifter output.
fn flatten_add_tree(
    graph: &Graph,
    out: NodeOutputId,
    acc: &mut Vec<NodeOutputId>,
    budget: &mut usize,
) {
    if *budget >= 32 {
        acc.push(out);
        return;
    }
    *budget += 1;
    let node = graph.get_node_from_output(out);
    // `addr -= K` from arm/arm-thumb stack-array dispatch lowering arrives
    // as `Add(addr, Neg(IntConst(K)))` (or the post-fold
    // `Add(addr, IntConst(-K))`).  `int_const_signed` sees through
    // `Neg(IntConst)`, so the per-term decompose step downstream catches
    // that constant via the `None` arm at line ~307.
    if let (NodeKind::IntBinaryOp(IntBinaryOp::Add), Ok([lhs, rhs])) = (
        graph.node_kind(node),
        graph.node_inputs_exact::<2>(node),
    ) {
        flatten_add_tree(graph, lhs, acc, budget);
        flatten_add_tree(graph, rhs, acc, budget);
        return;
    }
    acc.push(out);
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
    fg: &BuiltFunctionGraph,
    candidate: NodeOutputId,
) -> Option<(NodeOutputId, u64)> {
    // CORRECTNESS — pattern-DSL form replaces the prior arm-by-arm
    // dispatch on `NodeKind`.  `pattern::mul` is auto-commutative,
    // collapsing the prior `(idx, IntConst)` / `(IntConst, idx)` arms
    // into one pattern.  `pattern::shl` keeps stated order (shifts
    // are non-commutative) — the rhs must still be the const stride
    // exponent.  We try the multiplication shape first, then the
    // shift shape, mirroring the prior match's arm order.
    use pattern::{Capture, Matcher, any_int_const, mul, shl, var};

    let candidate_node = fg.graph.get_node_from_output(candidate);
    let matcher = Matcher::new(fg);

    // Mul(idx, IntConst(stride)) — either ordering.
    let stride_var = Capture::new();
    let idx_var = Capture::new();
    let mul_pat = mul(var(idx_var), any_int_const(stride_var));
    if let Some(m) = matcher.match_at(candidate_node, &mul_pat.into()) {
        let stride_u128 = m.get_uint(stride_var, fg)?;
        // `get_uint` returns `u128`; the prior code's `int_const_val`
        // truncated to `u64`.  Mirror that here.  Real strides fit
        // in `u64` everywhere we run.
        #[allow(clippy::cast_possible_truncation)]
        let stride = stride_u128 as u64;
        let idx = m.output(idx_var)?;
        return Some((idx, stride));
    }

    // ShiftLeft(idx, IntConst(s)) — non-commutative; rhs must be const.
    let s_var = Capture::new();
    let idx_var = Capture::new();
    let shl_pat = shl(var(idx_var), any_int_const(s_var));
    let m = matcher.match_at(candidate_node, &shl_pat.into())?;
    let s_u128 = m.get_uint(s_var, fg)?;
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
    let idx = m.output(idx_var)?;
    Some((idx, stride))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, dead_code)]

    use super::*;
    use ir::node::NodeOutputType;
    use ir::{ExtendOp, FunctionBuilder};
    use crate::{ConstantFold, KnownBits, OptimizerPipeline, RedundantPhis, StackStoreDetect};

    fn sp64() -> rsleigh::Vn {
        rsleigh::Vn {
            addr_off: 0x40,
            addr_space: rsleigh::VnSpace::REGISTER,
            size: 8,
        }
    }

    fn build_two_target_array(
        targets: [u64; 2],
        base_offset: i64,
        stride: u64,
    ) -> (ir::BuiltFunctionGraph, NodeOutputId) {
        let sp = sp64();
        let arg_vn = rsleigh::Vn {
            addr_off: 0x38,
            addr_space: rsleigh::VnSpace::REGISTER,
            size: 8,
        };
        let mut b = FunctionBuilder::new_raw(vec![sp, arg_vn], &[], &[sp], &[], None, 0).unwrap();
        let region = b.create_region().unwrap();
        b.set_entry_region(region).unwrap();
        b.set_region(region);
        let sp_val = b.read_variable(&sp).unwrap();
        for (i, &target_addr) in targets.iter().enumerate() {
            let off = base_offset + (i as i64) * (stride as i64);
            let off_const = b.build_int_const(off as u64, NodeOutputType::U64).unwrap();
            let addr = b
                .build_int_binary_operation(sp_val, off_const, IntBinaryOp::Add, NodeOutputType::U64)
                .unwrap();
            let target = b.build_int_const(target_addr, NodeOutputType::U64).unwrap();
            b.build_store(addr, target, rsleigh::VnSpace::RAM).unwrap();
        }
        let arg_val = b.read_variable(&arg_vn).unwrap();
        let arg_u32 = b.graph_mut().create_node(
            NodeKind::Truncate,
            [arg_val],
            [ir::node::NodeOutputKind::OutputType(NodeOutputType::U32)],
        );
        let arg_u32_out = b.body().graph.node_outputs_exact::<1>(arg_u32).unwrap()[0];
        let one = b.build_int_const(1u64, NodeOutputType::U32).unwrap();
        let masked = b
            .build_int_binary_operation(arg_u32_out, one, IntBinaryOp::And, NodeOutputType::U32)
            .unwrap();
        let idx_u64 = b.graph_mut().create_node(
            NodeKind::Extend(ExtendOp::ZeroExtend),
            [masked],
            [ir::node::NodeOutputKind::OutputType(NodeOutputType::U64)],
        );
        let idx_u64_out = b.body().graph.node_outputs_exact::<1>(idx_u64).unwrap()[0];
        let stride_const = b.build_int_const(stride, NodeOutputType::U64).unwrap();
        let idx_scaled = b
            .build_int_binary_operation(idx_u64_out, stride_const, IntBinaryOp::Mul, NodeOutputType::U64)
            .unwrap();
        let base_const = b.build_int_const(base_offset as u64, NodeOutputType::U64).unwrap();
        let sp_plus_base = b
            .build_int_binary_operation(sp_val, base_const, IntBinaryOp::Add, NodeOutputType::U64)
            .unwrap();
        let load_addr = b
            .build_int_binary_operation(sp_plus_base, idx_scaled, IntBinaryOp::Add, NodeOutputType::U64)
            .unwrap();
        let loaded = b
            .build_load(load_addr, rsleigh::VnSpace::RAM, NodeOutputType::U64)
            .unwrap();
        b.build_return(Some(loaded), &[]).unwrap();
        let mut fg = b.build().unwrap();
        let mut p = OptimizerPipeline::new();
        p.add(ConstantFold);
        p.add(KnownBits);
        p.add(RedundantPhis);
        p.add(StackStoreDetect::new(sp));
        p.run(&mut fg.graph, fg.entry).unwrap();
        let load = fg
            .all_node_ids()
            .find(|&n| matches!(fg.graph.node_kind(n), NodeKind::Load(_)))
            .expect("Load survives — StackLoadForward not in pipeline");
        let load_out = fg.graph.node_outputs_exact::<1>(load).unwrap()[0];
        (fg, load_out)
    }

    #[test]
    fn classify_stack_array_two_targets_resolves() {
        let targets = [0x401190u64, 0x401180u64];
        let (fg, load_out) = build_two_target_array(targets, -24, 8);
        let known = crate::analyze_known_bits(&fg).expect("kb analyze");
        let result = classify_stack_array(&fg, load_out, sp64(), &known);
        let mut expected = targets.to_vec();
        expected.sort_unstable();
        assert_eq!(result, Some(ResolvedTargets::Multiple(expected)));
    }

    #[test]
    fn classify_stack_array_returns_none_on_non_indexed_load() {
        let sp = sp64();
        let mut b = FunctionBuilder::new_raw(vec![sp], &[], &[sp], &[], None, 0).unwrap();
        let region = b.create_region().unwrap();
        b.set_entry_region(region).unwrap();
        b.set_region(region);
        let sp_val = b.read_variable(&sp).unwrap();
        let off = b.build_int_const(24u64, NodeOutputType::U64).unwrap();
        let addr = b
            .build_int_sub(sp_val, off, NodeOutputType::U64)
            .unwrap();
        let v = b.build_int_const(0xCAFEu64, NodeOutputType::U64).unwrap();
        b.build_store(addr, v, rsleigh::VnSpace::RAM).unwrap();
        let loaded = b.build_load(addr, rsleigh::VnSpace::RAM, NodeOutputType::U64).unwrap();
        b.build_return(Some(loaded), &[]).unwrap();
        let mut fg = b.build().unwrap();
        let mut p = OptimizerPipeline::new();
        p.add(ConstantFold);
        p.add(KnownBits);
        p.add(RedundantPhis);
        p.add(StackStoreDetect::new(sp));
        p.run(&mut fg.graph, fg.entry).unwrap();
        let load = fg
            .all_node_ids()
            .find(|&n| matches!(fg.graph.node_kind(n), NodeKind::Load(_)))
            .unwrap();
        let load_out = fg.graph.node_outputs_exact::<1>(load).unwrap()[0];
        let known = crate::analyze_known_bits(&fg).expect("kb analyze");
        assert_eq!(classify_stack_array(&fg, load_out, sp64(), &known), None);
    }

    #[test]
    fn classify_stack_array_returns_none_on_unbounded_idx() {
        let sp = sp64();
        let arg_vn = rsleigh::Vn {
            addr_off: 0x38,
            addr_space: rsleigh::VnSpace::REGISTER,
            size: 8,
        };
        let mut b = FunctionBuilder::new_raw(vec![sp, arg_vn], &[], &[sp], &[], None, 0).unwrap();
        let region = b.create_region().unwrap();
        b.set_entry_region(region).unwrap();
        b.set_region(region);
        let sp_val = b.read_variable(&sp).unwrap();
        let off24 = b.build_int_const(24u64, NodeOutputType::U64).unwrap();
        let addr_24 = b
            .build_int_sub(sp_val, off24, NodeOutputType::U64)
            .unwrap();
        let v = b.build_int_const(0x1234u64, NodeOutputType::U64).unwrap();
        b.build_store(addr_24, v, rsleigh::VnSpace::RAM).unwrap();
        let arg_val = b.read_variable(&arg_vn).unwrap();
        let stride = b.build_int_const(8u64, NodeOutputType::U64).unwrap();
        let idx_scaled = b
            .build_int_binary_operation(arg_val, stride, IntBinaryOp::Mul, NodeOutputType::U64)
            .unwrap();
        let base = b.build_int_const((-24i64) as u64, NodeOutputType::U64).unwrap();
        let sp_plus_base = b
            .build_int_binary_operation(sp_val, base, IntBinaryOp::Add, NodeOutputType::U64)
            .unwrap();
        let load_addr = b
            .build_int_binary_operation(sp_plus_base, idx_scaled, IntBinaryOp::Add, NodeOutputType::U64)
            .unwrap();
        let loaded = b
            .build_load(load_addr, rsleigh::VnSpace::RAM, NodeOutputType::U64)
            .unwrap();
        b.build_return(Some(loaded), &[]).unwrap();
        let mut fg = b.build().unwrap();
        let mut p = OptimizerPipeline::new();
        p.add(ConstantFold);
        p.add(KnownBits);
        p.add(RedundantPhis);
        p.add(StackStoreDetect::new(sp));
        p.run(&mut fg.graph, fg.entry).unwrap();
        let load = fg
            .all_node_ids()
            .find(|&n| matches!(fg.graph.node_kind(n), NodeKind::Load(_)))
            .unwrap();
        let load_out = fg.graph.node_outputs_exact::<1>(load).unwrap()[0];
        let known = crate::analyze_known_bits(&fg).expect("kb analyze");
        assert_eq!(classify_stack_array(&fg, load_out, sp64(), &known), None);
    }

    // ── strip_target_mask characterization tests ──────────────────
    //
    // These tests pin the contract of `strip_target_mask` before R2's
    // refactor migrates the manual NodeKind matching to `pattern::and` /
    // `pattern::or`.  The pre-refactor implementation hand-rolled
    // commutative operand checks; pattern's auto-commutative `and` /
    // `or` express the same shape with auto-handled operand swapping.
    // We therefore pin both operand orderings explicitly so the
    // refactor cannot accidentally narrow what we accept.
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
    /// Returns `(graph, anchor_output)`.  The anchor must NOT itself be
    /// an `IntConst`, because `strip_target_mask`'s commutative-And
    /// handling captures the const operand on either side; an IntConst
    /// inner would incorrectly pin the captured "non-const" side.
    fn build_load_anchor() -> (ir::BuiltFunctionGraph, NodeOutputId) {
        let reg = rsleigh::Vn {
            addr_off: 0x10,
            addr_space: rsleigh::VnSpace::REGISTER,
            size: 8,
        };
        let mut b =
            FunctionBuilder::new_raw(vec![reg], &[], &[], &[], None, 0).unwrap();
        let region = b.create_region().unwrap();
        b.set_entry_region(region).unwrap();
        b.set_region(region);
        let addr = b.read_variable(&reg).unwrap();
        let v = b
            .build_load(addr, rsleigh::VnSpace::RAM, NodeOutputType::U64)
            .unwrap();
        b.build_return(Some(v), &[]).unwrap();
        let fg = b.build().unwrap();
        (fg, v)
    }

    /// Wraps `inner` in `IntBinaryOp(op)` with the given side-ordering of
    /// the constant `c`.  `swap=false` produces `op(inner, IntConst(c))`;
    /// `swap=true` produces `op(IntConst(c), inner)`.  `ty` is the output
    /// type of both operands and the result.
    fn build_binop_wrapped(
        graph: &mut ir::Graph,
        inner: NodeOutputId,
        op: IntBinaryOp,
        c: u64,
        ty: NodeOutputType,
        swap: bool,
    ) -> NodeOutputId {
        let const_node = graph.create_node(
            NodeKind::IntConst(u128::from(c)),
            [],
            [ir::node::NodeOutputKind::OutputType(ty)],
        );
        let const_out = graph.node_outputs_exact::<1>(const_node).unwrap()[0];
        let (lhs, rhs) = if swap { (const_out, inner) } else { (inner, const_out) };
        let n = graph.create_node(
            NodeKind::IntBinaryOp(op),
            [lhs, rhs],
            [ir::node::NodeOutputKind::OutputType(ty)],
        );
        graph.node_outputs_exact::<1>(n).unwrap()[0]
    }

    #[test]
    fn strip_target_mask_no_wrapper_returns_all_ones() {
        let (fg, anchor) = build_load_anchor();
        let (out, mask) = strip_target_mask(&fg, anchor);
        assert_eq!(out, anchor, "no wrapper: anchor passes through");
        assert_eq!(mask, !0u64, "no wrapper: mask must be all-ones");
    }

    #[test]
    fn strip_target_mask_and_with_const_rhs_strips_one_layer() {
        let (mut fg, inner) = build_load_anchor();
        let wrapped = build_binop_wrapped(
            &mut fg.graph, inner, IntBinaryOp::And, 0xFFFE, NodeOutputType::U64, false,
        );
        let (out, mask) = strip_target_mask(&fg, wrapped);
        assert_eq!(out, inner, "And(load, K) strips to load");
        assert_eq!(mask, 0xFFFE, "And(load, K) yields mask K");
    }

    #[test]
    fn strip_target_mask_and_with_const_lhs_strips_one_layer() {
        let (mut fg, inner) = build_load_anchor();
        let wrapped = build_binop_wrapped(
            &mut fg.graph, inner, IntBinaryOp::And, 0xFFFE, NodeOutputType::U64, true,
        );
        let (out, mask) = strip_target_mask(&fg, wrapped);
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
        let or_layer = build_binop_wrapped(
            &mut fg.graph, inner, IntBinaryOp::Or, 1, NodeOutputType::U64, false,
        );
        let and_layer = build_binop_wrapped(
            &mut fg.graph, or_layer, IntBinaryOp::And, 0xFFFE, NodeOutputType::U64, false,
        );
        let (out, mask) = strip_target_mask(&fg, and_layer);
        assert_eq!(out, inner, "And(Or(load, 1), 0xFFFE) strips both wrappers");
        assert_eq!(mask, 0xFFFE, "and-then-or yields the And's mask");
    }

    #[test]
    fn strip_target_mask_or_overlapping_mask_stops_at_or() {
        // The Or's constant overlaps with surviving mask bits, so the
        // strip must NOT pass through it.  The Or stays in place;
        // the surrounding And contributes its mask.
        let (mut fg, inner) = build_load_anchor();
        let or_layer = build_binop_wrapped(
            &mut fg.graph, inner, IntBinaryOp::Or, 0xFF, NodeOutputType::U64, false,
        );
        let and_layer = build_binop_wrapped(
            &mut fg.graph, or_layer, IntBinaryOp::And, 0xFFFE, NodeOutputType::U64, false,
        );
        let (out, mask) = strip_target_mask(&fg, and_layer);
        assert_eq!(out, or_layer, "overlapping Or is preserved");
        assert_eq!(mask, 0xFFFE, "And's mask still applies");
    }

    #[test]
    fn strip_target_mask_nested_ands_compose_via_intersection() {
        // And(And(inner, 0xFFFF), 0xFF) — the second And narrows further.
        // Both layers strip; surviving mask is the intersection.
        let (mut fg, inner) = build_load_anchor();
        let inner_and = build_binop_wrapped(
            &mut fg.graph, inner, IntBinaryOp::And, 0xFFFF, NodeOutputType::U64, false,
        );
        let outer_and = build_binop_wrapped(
            &mut fg.graph, inner_and, IntBinaryOp::And, 0xFF, NodeOutputType::U64, false,
        );
        let (out, mask) = strip_target_mask(&fg, outer_and);
        assert_eq!(out, inner, "nested Ands strip down to innermost");
        assert_eq!(mask, 0xFF, "nested Ands intersect their masks");
    }
}
