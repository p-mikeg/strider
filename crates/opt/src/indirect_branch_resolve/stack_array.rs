//! BUG-30 — stack-array-of-labels arm of the tier-2 indirect-branch classifier.
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
//!     `BranchResolution::Multiple([c0, c1, ...])`.
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

use super::BranchResolution;
use ir::node::{NodeKind, NodeOutputId};
use ir::{Graph, IntBinaryOp};
use crate::sp_expr::{SpExpr, SpExprMemo, decompose_sp};
use crate::stack_load_forward::find_stack_stored_value_at_offset;

use super::jump_table::{bound_via_known_bits, bound_via_predecessor_if};

/// Per-call enumeration cap.  Mirrors the rodata jump-table arm's
/// `MAX_TABLE_ENTRIES` for the same reason: a buggy KnownBits result
/// could otherwise force iteration through 4 GiB of slots.  Real
/// `goto *targets[]` arrays are bounded by the source-level switch arm
/// count, well under 4096.
const MAX_TABLE_ENTRIES: u64 = 4096;

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
    graph: &Graph,
    anchor_output: NodeOutputId,
    stack_ptr_vn: rsleigh::Vn,
) -> Option<BranchResolution> {
    // ARM/Thumb interworking strips the LSB Thumb-mode marker from the
    // dispatch target via `IntBinaryOp(And)` with a constant mask
    // (`& 0xFFFFFFFE` for 32-bit ARM, `& 0xFFFFFFFFFFFFFFFE` for 64-bit
    // archs that interwork through the same idiom).  The Load at the
    // dispatch site is the `lhs` of that And; we transparently look
    // through the wrapper, run the rest of the classification on the
    // underlying Load, and `& mask` each enumerated target before
    // returning.  Non-And anchors take the path with `mask = !0`.
    let (load_anchor, target_mask) = strip_target_mask(graph, anchor_output);

    let shape = match_stack_array_shape(graph, load_anchor, stack_ptr_vn)?;
    let bound = bound_via_known_bits(graph, shape.idx_output)
        .or_else(|| bound_via_predecessor_if(graph, anchor_output, shape.idx_output))?;
    if bound == 0 || bound > MAX_TABLE_ENTRIES {
        return None;
    }
    let mut memo = SpExprMemo::default();
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
        )?;
        let c = graph.int_const_val(value)?;
        targets.push(c & target_mask);
    }
    targets.sort_unstable();
    targets.dedup();
    if targets.is_empty() {
        None
    } else {
        Some(BranchResolution::Multiple(targets))
    }
}

/// Strip a top-level `IntBinaryOp(And)` whose mask is a constant —
/// returns the underlying value-output and the (u64-truncated) mask.
/// When the anchor isn't an `And`, returns `(anchor_output, !0u64)` so
/// the caller's masking step is a no-op.
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
fn strip_target_mask(
    graph: &Graph,
    anchor_output: NodeOutputId,
) -> (NodeOutputId, u64) {
    let mut current = anchor_output;
    let mut mask: u64 = !0u64;
    // Strip up to a fixed number of layers; ARM-Thumb commonly nests
    // `And(Or(load, 1), 0xFFFFFFFE)` (set LSB then mask it off) — that's
    // 2 layers.  Cap at 4 to defend against pathologically deep wrappers
    // from buggy lifter output.
    for _ in 0..4 {
        let producer = graph.get_node_from_output(current);
        match graph.node_kind(producer) {
            // And-with-constant: mask narrows.
            NodeKind::IntBinaryOp(IntBinaryOp::And) => {
                if let Ok([lhs, rhs]) = graph.node_inputs_exact::<2>(producer) {
                    if let Some(m) = graph.int_const_val(rhs) {
                        mask &= m;
                        current = lhs;
                        continue;
                    }
                    if let Some(m) = graph.int_const_val(lhs) {
                        mask &= m;
                        current = rhs;
                        continue;
                    }
                }
                break;
            }
            // Or-with-constant: when the OR's constant is fully covered
            // by the bits we'll later mask off (i.e. `or_const & mask
            // == 0`), the OR is a no-op for the dispatch target — strip
            // it transparently.  Common in ARM-Thumb: `Or(load, 1)`
            // followed by `And(_, 0xFFFFFFFE)` — the OR sets bit 0, the
            // AND clears it.  When the OR's constant overlaps with
            // surviving mask bits, leave the wrapper in place (the
            // shape match below will fail and we defer to the
            // orchestrator).
            NodeKind::IntBinaryOp(IntBinaryOp::Or) => {
                if let Ok([lhs, rhs]) = graph.node_inputs_exact::<2>(producer) {
                    let or_const = graph
                        .int_const_val(rhs)
                        .map(|c| (c, lhs))
                        .or_else(|| graph.int_const_val(lhs).map(|c| (c, rhs)));
                    if let Some((or_c, other)) = or_const {
                        // Strip iff every set bit of `or_c` is already
                        // cleared by `mask`.  This is precisely the
                        // case where the OR has no observable effect on
                        // the masked result.
                        if or_c & mask == 0 {
                            current = other;
                            continue;
                        }
                    }
                }
                break;
            }
            _ => break,
        }
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
    graph: &Graph,
    anchor_output: NodeOutputId,
    stack_ptr_vn: rsleigh::Vn,
) -> Option<StackArrayShape> {
    let load_node = graph.get_node_from_output(anchor_output);
    let NodeKind::Load(_) = *graph.node_kind(load_node) else {
        return None;
    };
    let value_type = graph.output_kind(anchor_output).as_value()?;
    if !value_type.is_integer() {
        return None;
    }
    let load_inputs: Vec<NodeOutputId> =
        graph.node_inputs(load_node).into_iter().collect();
    let mem_input = *load_inputs.first()?;
    let addr_output = *load_inputs.get(1)?;

    // Flatten the address into a sum of terms.  ARM lifters sometimes
    // emit `Add(Add(sp, idx*stride), const)` (a nested Add tree)
    // instead of the flat `Add(sp + const, idx*stride)` that x86 / x64
    // produce.  Walk every `Add` / `Sub` node transitively to collect
    // the additive operands.
    let mut terms: Vec<NodeOutputId> = Vec::new();
    let mut sub_const_offset: i64 = 0;
    flatten_add_tree(graph, addr_output, &mut terms, &mut sub_const_offset, &mut 0);

    // Among the terms, exactly one must be a `Mul`/`ShiftLeft` shape
    // we can crack into (idx, stride).  The rest must sum (with
    // `decompose_sp`) to `Terminal { offset: K }`.
    let mut idx_stride: Option<(NodeOutputId, u64, usize)> = None;
    for (i, t) in terms.iter().enumerate() {
        if let Some((idx, stride)) = extract_idx_and_stride(graph, *t) {
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
    // Seed the offset accumulator with the constant-rhs Sub adjustment
    // that `flatten_add_tree` rolled up while walking.
    let mut base_offset_acc: i64 = sub_const_offset;
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
                // single-region BUG-30 shape.  Bail.
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
    extra_offset: &mut i64,
    budget: &mut usize,
) {
    if *budget >= 32 {
        acc.push(out);
        return;
    }
    *budget += 1;
    let node = graph.get_node_from_output(out);
    match graph.node_kind(node) {
        NodeKind::IntBinaryOp(IntBinaryOp::Add) => {
            if let Ok([lhs, rhs]) = graph.node_inputs_exact::<2>(node) {
                flatten_add_tree(graph, lhs, acc, extra_offset, budget);
                flatten_add_tree(graph, rhs, acc, extra_offset, budget);
                return;
            }
        }
        NodeKind::IntBinaryOp(IntBinaryOp::Sub) => {
            if let Ok([lhs, rhs]) = graph.node_inputs_exact::<2>(node) {
                // Only handle Sub with a constant rhs (the common
                // "addr -= K" idiom from arm/arm-thumb stack-array
                // dispatch lowering).  Negate the constant and roll it
                // into extra_offset; recurse on lhs.  When rhs is
                // non-constant, push the Sub unmodified — the per-term
                // decompose step downstream will fail closed.
                if let Some(c) = crate::sp_expr::int_const_signed(graph, rhs) {
                    *extra_offset = extra_offset.wrapping_sub(c);
                    flatten_add_tree(graph, lhs, acc, extra_offset, budget);
                    return;
                }
            }
        }
        _ => {}
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
    graph: &Graph,
    candidate: NodeOutputId,
) -> Option<(NodeOutputId, u64)> {
    let node = graph.get_node_from_output(candidate);
    match graph.node_kind(node) {
        NodeKind::IntBinaryOp(IntBinaryOp::Mul) => {
            let [lhs, rhs] = graph.node_inputs_exact::<2>(node).ok()?;
            if let Some(stride) = graph.int_const_val(rhs) {
                return Some((lhs, stride));
            }
            if let Some(stride) = graph.int_const_val(lhs) {
                return Some((rhs, stride));
            }
            None
        }
        NodeKind::IntBinaryOp(IntBinaryOp::ShiftLeft) => {
            let [lhs, rhs] = graph.node_inputs_exact::<2>(node).ok()?;
            let s = graph.int_const_val(rhs)?;
            if s >= 64 {
                return None;
            }
            // s is bounded above by 64 → fits in u32 with no truncation.
            let s32 = u32::try_from(s).ok()?;
            let stride = 1u64.checked_shl(s32)?;
            Some((lhs, stride))
        }
        _ => None,
    }
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
            addr: rsleigh::VnAddr {
                space: rsleigh::VnSpace::REGISTER,
                off: 0x40,
            },
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
            addr: rsleigh::VnAddr {
                space: rsleigh::VnSpace::REGISTER,
                off: 0x38,
            },
            size: 8,
        };
        let mut b = FunctionBuilder::new_raw(vec![sp, arg_vn], &[], &[sp], &[], None, 0).unwrap();
        let region = b.create_region().unwrap();
        b.set_entry_region(region).unwrap();
        b.set_region(region);
        let sp_val = b.read_variable(&sp).unwrap();
        for (i, &target_addr) in targets.iter().enumerate() {
            let off = base_offset + (i as i64) * (stride as i64);
            let off_const = b.build_int_const(off as u64, NodeOutputType::U64);
            let addr = b
                .build_int_binary_operation(sp_val, off_const, IntBinaryOp::Add, NodeOutputType::U64)
                .unwrap();
            let target = b.build_int_const(target_addr, NodeOutputType::U64);
            b.build_store(addr, target, rsleigh::VnSpace::RAM).unwrap();
        }
        let arg_val = b.read_variable(&arg_vn).unwrap();
        let arg_u32 = b.graph_mut().create_node(
            NodeKind::Truncate,
            [arg_val],
            [ir::node::NodeOutputKind::OutputType(NodeOutputType::U32)],
        );
        let arg_u32_out = b.body().graph.node_outputs_exact::<1>(arg_u32).unwrap()[0];
        let one = b.build_int_const(1u64, NodeOutputType::U32);
        let masked = b
            .build_int_binary_operation(arg_u32_out, one, IntBinaryOp::And, NodeOutputType::U32)
            .unwrap();
        let idx_u64 = b.graph_mut().create_node(
            NodeKind::Extend(ExtendOp::ZeroExtend),
            [masked],
            [ir::node::NodeOutputKind::OutputType(NodeOutputType::U64)],
        );
        let idx_u64_out = b.body().graph.node_outputs_exact::<1>(idx_u64).unwrap()[0];
        let stride_const = b.build_int_const(stride, NodeOutputType::U64);
        let idx_scaled = b
            .build_int_binary_operation(idx_u64_out, stride_const, IntBinaryOp::Mul, NodeOutputType::U64)
            .unwrap();
        let base_const = b.build_int_const(base_offset as u64, NodeOutputType::U64);
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
        let result = classify_stack_array(&fg.graph, load_out, sp64());
        let mut expected = targets.to_vec();
        expected.sort_unstable();
        assert_eq!(result, Some(BranchResolution::Multiple(expected)));
    }

    #[test]
    fn classify_stack_array_returns_none_on_non_indexed_load() {
        let sp = sp64();
        let mut b = FunctionBuilder::new_raw(vec![sp], &[], &[sp], &[], None, 0).unwrap();
        let region = b.create_region().unwrap();
        b.set_entry_region(region).unwrap();
        b.set_region(region);
        let sp_val = b.read_variable(&sp).unwrap();
        let off = b.build_int_const(24u64, NodeOutputType::U64);
        let addr = b
            .build_int_binary_operation(sp_val, off, IntBinaryOp::Sub, NodeOutputType::U64)
            .unwrap();
        let v = b.build_int_const(0xCAFEu64, NodeOutputType::U64);
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
        assert_eq!(classify_stack_array(&fg.graph, load_out, sp64()), None);
    }

    #[test]
    fn classify_stack_array_returns_none_on_unbounded_idx() {
        let sp = sp64();
        let arg_vn = rsleigh::Vn {
            addr: rsleigh::VnAddr {
                space: rsleigh::VnSpace::REGISTER,
                off: 0x38,
            },
            size: 8,
        };
        let mut b = FunctionBuilder::new_raw(vec![sp, arg_vn], &[], &[sp], &[], None, 0).unwrap();
        let region = b.create_region().unwrap();
        b.set_entry_region(region).unwrap();
        b.set_region(region);
        let sp_val = b.read_variable(&sp).unwrap();
        let off24 = b.build_int_const(24u64, NodeOutputType::U64);
        let addr_24 = b
            .build_int_binary_operation(sp_val, off24, IntBinaryOp::Sub, NodeOutputType::U64)
            .unwrap();
        let v = b.build_int_const(0x1234u64, NodeOutputType::U64);
        b.build_store(addr_24, v, rsleigh::VnSpace::RAM).unwrap();
        let arg_val = b.read_variable(&arg_vn).unwrap();
        let stride = b.build_int_const(8u64, NodeOutputType::U64);
        let idx_scaled = b
            .build_int_binary_operation(arg_val, stride, IntBinaryOp::Mul, NodeOutputType::U64)
            .unwrap();
        let base = b.build_int_const((-24i64) as u64, NodeOutputType::U64);
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
        assert_eq!(classify_stack_array(&fg.graph, load_out, sp64()), None);
    }
}
