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

use cfg::test_api::ResolvedTargets;
use ir::node::{NodeKind, NodeOutputId};
use ir::{BuiltFunctionGraph, IntBinaryOp};
use opt::sp_expr::{SpExpr, SpExprMemo, decompose_sp};
use opt::stack_load_forward::find_stack_stored_value_at_offset;

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
    graph: &BuiltFunctionGraph,
    anchor_output: NodeOutputId,
    stack_ptr_vn: rsleigh::Vn,
) -> Option<ResolvedTargets> {
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
            &graph.graph,
            shape.mem_input,
            off,
            shape.value_type,
            stack_ptr_vn,
            &mut memo,
        )?;
        let c = graph.int_const_val(value)?;
        #[allow(clippy::cast_possible_truncation)]
        let masked = (c as u64) & target_mask;
        targets.push(masked);
    }
    targets.sort_unstable();
    targets.dedup();
    if targets.is_empty() {
        None
    } else {
        Some(ResolvedTargets::Multiple(targets))
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
    graph: &BuiltFunctionGraph,
    anchor_output: NodeOutputId,
) -> (NodeOutputId, u64) {
    let producer = graph.graph.get_node_from_output(anchor_output);
    if let NodeKind::IntBinaryOp(IntBinaryOp::And) = graph.graph.node_kind(producer) {
        if let Ok([lhs, rhs]) = graph.graph.node_inputs_exact::<2>(producer) {
            // Either operand may be the constant mask.
            if let Some(m) = graph.int_const_val(rhs) {
                #[allow(clippy::cast_possible_truncation)]
                return (lhs, m as u64);
            }
            if let Some(m) = graph.int_const_val(lhs) {
                #[allow(clippy::cast_possible_truncation)]
                return (rhs, m as u64);
            }
        }
    }
    (anchor_output, !0u64)
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
    graph: &BuiltFunctionGraph,
    anchor_output: NodeOutputId,
    stack_ptr_vn: rsleigh::Vn,
) -> Option<StackArrayShape> {
    let load_node = graph.graph.get_node_from_output(anchor_output);
    let NodeKind::Load(_) = *graph.graph.node_kind(load_node) else {
        return None;
    };
    let value_type = graph.graph.output_kind(anchor_output).as_value()?;
    if !value_type.is_integer() {
        return None;
    }
    let load_inputs: Vec<NodeOutputId> =
        graph.graph.node_inputs(load_node).into_iter().collect();
    let mem_input = *load_inputs.first()?;
    let addr_output = *load_inputs.get(1)?;
    let add_node = graph.graph.get_node_from_output(addr_output);
    if !matches!(
        graph.graph.node_kind(add_node),
        NodeKind::IntBinaryOp(IntBinaryOp::Add)
    ) {
        return None;
    }
    let [add_lhs, add_rhs] = graph.graph.node_inputs_exact::<2>(add_node).ok()?;
    extract_sp_and_mul(graph, add_lhs, add_rhs, value_type, mem_input, stack_ptr_vn)
        .or_else(|| extract_sp_and_mul(graph, add_rhs, add_lhs, value_type, mem_input, stack_ptr_vn))
}

fn extract_sp_and_mul(
    graph: &BuiltFunctionGraph,
    sp_candidate: NodeOutputId,
    mul_candidate: NodeOutputId,
    value_type: ir::node::NodeOutputType,
    mem_input: NodeOutputId,
    stack_ptr_vn: rsleigh::Vn,
) -> Option<StackArrayShape> {
    let mut sp_memo = SpExprMemo::default();
    let mut sp_visiting = rustc_hash::FxHashSet::default();
    let SpExpr::Terminal { base: _, offset: base_offset } =
        decompose_sp(&graph.graph, sp_candidate, stack_ptr_vn, &mut sp_memo, &mut sp_visiting)?
    else {
        return None;
    };
    let (idx_output, stride) = extract_idx_and_stride(graph, mul_candidate)?;
    Some(StackArrayShape {
        base_offset,
        stride,
        idx_output,
        value_type,
        mem_input,
    })
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
    graph: &BuiltFunctionGraph,
    candidate: NodeOutputId,
) -> Option<(NodeOutputId, u64)> {
    let node = graph.graph.get_node_from_output(candidate);
    match graph.graph.node_kind(node) {
        NodeKind::IntBinaryOp(IntBinaryOp::Mul) => {
            let [lhs, rhs] = graph.graph.node_inputs_exact::<2>(node).ok()?;
            if let Some(stride) = graph.int_const_val(rhs) {
                return Some((lhs, stride as u64));
            }
            if let Some(stride) = graph.int_const_val(lhs) {
                return Some((rhs, stride as u64));
            }
            None
        }
        NodeKind::IntBinaryOp(IntBinaryOp::ShiftLeft) => {
            let [lhs, rhs] = graph.graph.node_inputs_exact::<2>(node).ok()?;
            let s = graph.int_const_val(rhs)?;
            if s >= 64 {
                return None;
            }
            #[allow(clippy::cast_possible_truncation)]
            let s32 = s as u32;
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
    use opt::{ConstantFold, KnownBits, OptimizerPipeline, RedundantPhis, StackStoreDetect};

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
        for i in 0..2 {
            let off = base_offset + (i as i64) * (stride as i64);
            let off_const = b.build_int_const(off as u64, NodeOutputType::U64);
            let addr = b
                .build_int_binary_operation(sp_val, off_const, IntBinaryOp::Add, NodeOutputType::U64)
                .unwrap();
            let target = b.build_int_const(targets[i], NodeOutputType::U64);
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
        let result = classify_stack_array(&fg, load_out, sp64());
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
        assert_eq!(classify_stack_array(&fg, load_out, sp64()), None);
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
        assert_eq!(classify_stack_array(&fg, load_out, sp64()), None);
    }
}
