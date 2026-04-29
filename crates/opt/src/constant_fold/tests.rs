use super::*;
use anyhow::anyhow;
use ir::node::{NodeKind, NodeOutputType};
use ir::{
    BoolBinaryOp, BoolUnaryOp, FloatBinaryOp, FloatCmpOp, FloatUnaryOp, FunctionBuilder,
    IntBinaryOp, IntCmpOp,
};

/// Builds a minimal single-region function whose return value is produced
/// by `f`.  All nodes built by `f` are reachable from the entry.
fn make_fn<F>(f: F) -> Result<ir::BuiltFunctionGraph>
where
    F: FnOnce(&mut FunctionBuilder) -> Result<ir::Value>,
{
    let mut b = FunctionBuilder::new_raw(vec![], &[], &[], &[], None, 0)?;
    let region = b.create_region()?;
    b.set_entry_region(region)?;
    b.set_region(region);
    let val = f(&mut b)?;
    b.build_return(Some(val), &[])?;
    b.build()
}

/// Returns the output id that the Return node receives as its value
/// argument (input[2]: input[0] is the control edge, input[1] is memory).
fn return_value(fg: &ir::BuiltFunctionGraph) -> Result<ir::Value> {
    let ret = fg
        .all_node_ids()
        .find(|&n| matches!(fg.graph.node_kind(n), NodeKind::Return))
        .ok_or_else(|| anyhow!("no return node found in function"))?;
    Ok(fg.graph.node_inputs(ret)[2])
}

/// Returns the `NodeKind` of the node that produces the return value.
fn return_kind(fg: &ir::BuiltFunctionGraph) -> Result<NodeKind> {
    let val = return_value(fg)?;
    Ok(*fg.graph.kind_of_output(val))
}

// ── integer binary folding ────────────────────────────────────────────────

#[test]
fn fold_int_add_consts() -> Result<()> {
    let mut fg = make_fn(|b| {
        let c3 = b.build_int_const(3u64, NodeOutputType::U64);
        let c4 = b.build_int_const(4u64, NodeOutputType::U64);
        b.build_int_binary_operation(c3, c4, IntBinaryOp::Add, NodeOutputType::U64)
    })?;
    assert!(ConstantFold.optimize(&mut fg.graph, fg.entry)?.changed());
    assert_eq!(return_kind(&fg)?, NodeKind::IntConst(7));
    Ok(())
}

#[test]
fn fold_int_and_zero() -> Result<()> {
    let mut fg = make_fn(|b| {
        let x = b.build_int_const(0xFFu64, NodeOutputType::U64);
        let zero = b.build_int_const(0u64, NodeOutputType::U64);
        b.build_int_binary_operation(x, zero, IntBinaryOp::And, NodeOutputType::U64)
    })?;
    assert!(ConstantFold.optimize(&mut fg.graph, fg.entry)?.changed());
    assert_eq!(return_kind(&fg)?, NodeKind::IntConst(0));
    Ok(())
}

#[test]
fn fold_int_xor_self() -> Result<()> {
    let mut fg = make_fn(|b| {
        let x = b.build_int_const(0xABu64, NodeOutputType::U64);
        b.build_int_binary_operation(x, x, IntBinaryOp::Xor, NodeOutputType::U64)
    })?;
    assert!(ConstantFold.optimize(&mut fg.graph, fg.entry)?.changed());
    assert_eq!(return_kind(&fg)?, NodeKind::IntConst(0));
    Ok(())
}

#[test]
fn fold_int_sub_self() -> Result<()> {
    let mut fg = make_fn(|b| {
        let x = b.build_int_const(0xABu64, NodeOutputType::U64);
        b.build_int_binary_operation(x, x, IntBinaryOp::Sub, NodeOutputType::U64)
    })?;
    assert!(ConstantFold.optimize(&mut fg.graph, fg.entry)?.changed());
    assert_eq!(return_kind(&fg)?, NodeKind::IntConst(0));
    Ok(())
}

#[test]
fn fold_add_zero_identity() -> Result<()> {
    // x + 0 → x  (x is non-const)
    let mut fg = make_fn(|b| {
        let c1 = b.build_int_const(1u64, NodeOutputType::U64);
        let c2 = b.build_int_const(2u64, NodeOutputType::U64);
        let x = b.build_int_binary_operation(c1, c2, IntBinaryOp::Add, NodeOutputType::U64)?;
        let zero = b.build_int_const(0u64, NodeOutputType::U64);
        b.build_int_binary_operation(x, zero, IntBinaryOp::Add, NodeOutputType::U64)
    })?;
    // After at least one fold pass x+0 should collapse to x, then x folds too.
    let mut changed = true;
    while changed {
        changed = ConstantFold.optimize(&mut fg.graph, fg.entry)?.changed();
    }
    assert_eq!(return_kind(&fg)?, NodeKind::IntConst(3));
    Ok(())
}

#[test]
fn fold_mul_by_one() -> Result<()> {
    let mut fg = make_fn(|b| {
        let c5 = b.build_int_const(5u64, NodeOutputType::U64);
        let one = b.build_int_const(1u64, NodeOutputType::U64);
        b.build_int_binary_operation(c5, one, IntBinaryOp::Mul, NodeOutputType::U64)
    })?;
    assert!(ConstantFold.optimize(&mut fg.graph, fg.entry)?.changed());
    assert_eq!(return_kind(&fg)?, NodeKind::IntConst(5));
    Ok(())
}

/// `(x & 4) & 7`  — bit 2 is the only bit reachable by both masks, so the
/// merged constant is `4 & 7 = 4`.
#[test]
fn fold_and_and_masks() -> Result<()> {
    let mut fg = make_fn(|b| {
        let x = b.build_int_const(0xFFu64, NodeOutputType::U64);
        let c4 = b.build_int_const(4u64, NodeOutputType::U64);
        let c7 = b.build_int_const(7u64, NodeOutputType::U64);
        let inner =
            b.build_int_binary_operation(x, c4, IntBinaryOp::And, NodeOutputType::U64)?;
        b.build_int_binary_operation(inner, c7, IntBinaryOp::And, NodeOutputType::U64)
    })?;
    // Run to convergence (both-const fold + mask-merge may each fire once).
    let mut changed = true;
    while changed {
        changed = ConstantFold.optimize(&mut fg.graph, fg.entry)?.changed();
    }
    // 0xFF & 4 = 4, 4 & 7 = 4.
    assert_eq!(return_kind(&fg)?, NodeKind::IntConst(4));
    Ok(())
}

// ── add/sub reassociation with constants ──────────────────────────────────

/// Fabricates a register varnode for use as a non-constant operand.
fn reg_vn(off: u64, size: u32) -> rsleigh::Vn {
    rsleigh::Vn {
        size,
        addr: rsleigh::VnAddr {
            off,
            space: rsleigh::VnSpace::REGISTER,
        },
    }
}

/// Builds a minimal function exposing a single tracked variable via
/// `read_variable` (which returns a `ControlPhi` output wrapping the
/// entry's `InitialVar`). The closure receives that non-constant value.
fn make_fn_with_var<F>(vn: rsleigh::Vn, f: F) -> Result<(ir::BuiltFunctionGraph, ir::Value)>
where
    F: FnOnce(&mut FunctionBuilder, ir::Value) -> Result<ir::Value>,
{
    let mut b = FunctionBuilder::new_raw(vec![vn], &[vn], &[], &[], None, 0)?;
    let region = b.create_region()?;
    b.set_entry_region(region)?;
    b.set_region(region);
    let x = b.read_variable(&vn)?;
    let val = f(&mut b, x)?;
    b.build_return(Some(val), &[])?;
    Ok((b.build()?, x))
}

/// Asserts the return-value node is `expected_base + expected_const`
/// (type-masked; operand order irrelevant).
fn assert_add_with_const(
    fg: &ir::BuiltFunctionGraph,
    expected_base: ir::Value,
    expected_const: u64,
    ty: NodeOutputType,
) -> Result<()> {
    let val = return_value(fg)?;
    let node = fg.graph.get_node_from_output(val);
    assert!(
        matches!(
            fg.graph.node_kind(node),
            NodeKind::IntBinaryOp(IntBinaryOp::Add)
        ),
        "expected outer Add, got {:?}",
        fg.graph.node_kind(node)
    );
    let inputs = fg.graph.node_inputs(node);
    assert_eq!(inputs.len(), 2);
    let l = inputs[0];
    let r = inputs[1];
    let masked = ty
        .get_unsigned_int(expected_const)
        .ok_or_else(|| anyhow!("expected integer type, got {ty:?}"))?;
    let const_on = |o: ir::Value| -> bool {
        matches!(
            *fg.graph.kind_of_output(o),
            // IntConst stores u128; masked is u64, widen for comparison.
            NodeKind::IntConst(v) if ty.get_unsigned_int_u128(v) == Some(u128::from(masked))
        )
    };
    let ok = (l == expected_base && const_on(r)) || (r == expected_base && const_on(l));
    assert!(
        ok,
        "expected `base + {:#x}`; got lhs kind={:?}, rhs kind={:?}",
        masked,
        fg.graph.kind_of_output(l),
        fg.graph.kind_of_output(r),
    );
    Ok(())
}

/// Asserts the return-value node is `expected_base - expected_const`
/// (lhs must be the base, rhs must be the constant; Sub is non-commutative).
fn assert_sub_with_const(
    fg: &ir::BuiltFunctionGraph,
    expected_base: ir::Value,
    expected_const: u64,
    ty: NodeOutputType,
) -> Result<()> {
    let val = return_value(fg)?;
    let node = fg.graph.get_node_from_output(val);
    assert!(
        matches!(
            fg.graph.node_kind(node),
            NodeKind::IntBinaryOp(IntBinaryOp::Sub)
        ),
        "expected outer Sub, got {:?}",
        fg.graph.node_kind(node)
    );
    let inputs = fg.graph.node_inputs(node);
    assert_eq!(inputs.len(), 2);
    let l = inputs[0];
    let r = inputs[1];
    let masked = ty
        .get_unsigned_int(expected_const)
        .ok_or_else(|| anyhow!("expected integer type, got {ty:?}"))?;
    let const_on_rhs = matches!(
        *fg.graph.kind_of_output(r),
        // IntConst stores u128; masked is u64, widen for comparison.
        NodeKind::IntConst(v) if ty.get_unsigned_int_u128(v) == Some(u128::from(masked))
    );
    assert!(
        l == expected_base && const_on_rhs,
        "expected `base - {:#x}`; got lhs kind={:?}, rhs kind={:?}",
        masked,
        fg.graph.kind_of_output(l),
        fg.graph.kind_of_output(r),
    );
    Ok(())
}

#[test]
fn reassoc_add_add_consts() -> Result<()> {
    // (x + 3) + 4 → x + 7
    let vn = reg_vn(0x1000, 8);
    let (mut fg, x) = make_fn_with_var(vn, |b, x| {
        let c3 = b.build_int_const(3u64, NodeOutputType::U64);
        let c4 = b.build_int_const(4u64, NodeOutputType::U64);
        let inner =
            b.build_int_binary_operation(x, c3, IntBinaryOp::Add, NodeOutputType::U64)?;
        b.build_int_binary_operation(inner, c4, IntBinaryOp::Add, NodeOutputType::U64)
    })?;
    let mut changed = true;
    while changed {
        changed = ConstantFold.optimize(&mut fg.graph, fg.entry)?.changed();
    }
    assert_add_with_const(&fg, x, 7, NodeOutputType::U64)?;
    Ok(())
}

#[test]
fn reassoc_add_sub_consts() -> Result<()> {
    // (x - 3) + 4 → x + 1
    let vn = reg_vn(0x1000, 8);
    let (mut fg, x) = make_fn_with_var(vn, |b, x| {
        let c3 = b.build_int_const(3u64, NodeOutputType::U64);
        let c4 = b.build_int_const(4u64, NodeOutputType::U64);
        let inner =
            b.build_int_binary_operation(x, c3, IntBinaryOp::Sub, NodeOutputType::U64)?;
        b.build_int_binary_operation(inner, c4, IntBinaryOp::Add, NodeOutputType::U64)
    })?;
    let mut changed = true;
    while changed {
        changed = ConstantFold.optimize(&mut fg.graph, fg.entry)?.changed();
    }
    assert_add_with_const(&fg, x, 1, NodeOutputType::U64)?;
    Ok(())
}

#[test]
fn reassoc_sub_add_consts_wrapping() -> Result<()> {
    // (x + 3) - 4 → x + (3 - 4)  = x + 0xFFFF_FFFF_FFFF_FFFF
    let vn = reg_vn(0x1000, 8);
    let (mut fg, x) = make_fn_with_var(vn, |b, x| {
        let c3 = b.build_int_const(3u64, NodeOutputType::U64);
        let c4 = b.build_int_const(4u64, NodeOutputType::U64);
        let inner =
            b.build_int_binary_operation(x, c3, IntBinaryOp::Add, NodeOutputType::U64)?;
        b.build_int_binary_operation(inner, c4, IntBinaryOp::Sub, NodeOutputType::U64)
    })?;
    let mut changed = true;
    while changed {
        changed = ConstantFold.optimize(&mut fg.graph, fg.entry)?.changed();
    }
    assert_add_with_const(&fg, x, 0xFFFF_FFFF_FFFF_FFFF, NodeOutputType::U64)?;
    Ok(())
}

#[test]
fn reassoc_sub_sub_consts() -> Result<()> {
    // (x - 3) - 4 → x - 7
    let vn = reg_vn(0x1000, 8);
    let (mut fg, x) = make_fn_with_var(vn, |b, x| {
        let c3 = b.build_int_const(3u64, NodeOutputType::U64);
        let c4 = b.build_int_const(4u64, NodeOutputType::U64);
        let inner =
            b.build_int_binary_operation(x, c3, IntBinaryOp::Sub, NodeOutputType::U64)?;
        b.build_int_binary_operation(inner, c4, IntBinaryOp::Sub, NodeOutputType::U64)
    })?;
    let mut changed = true;
    while changed {
        changed = ConstantFold.optimize(&mut fg.graph, fg.entry)?.changed();
    }
    assert_sub_with_const(&fg, x, 7, NodeOutputType::U64)?;
    Ok(())
}

#[test]
fn reassoc_add_commuted_inner() -> Result<()> {
    // (3 + x) + 4 → x + 7 (inner Add has const on lhs)
    let vn = reg_vn(0x1000, 8);
    let (mut fg, x) = make_fn_with_var(vn, |b, x| {
        let c3 = b.build_int_const(3u64, NodeOutputType::U64);
        let c4 = b.build_int_const(4u64, NodeOutputType::U64);
        let inner =
            b.build_int_binary_operation(c3, x, IntBinaryOp::Add, NodeOutputType::U64)?;
        b.build_int_binary_operation(inner, c4, IntBinaryOp::Add, NodeOutputType::U64)
    })?;
    let mut changed = true;
    while changed {
        changed = ConstantFold.optimize(&mut fg.graph, fg.entry)?.changed();
    }
    assert_add_with_const(&fg, x, 7, NodeOutputType::U64)?;
    Ok(())
}

#[test]
fn reassoc_add_commuted_outer() -> Result<()> {
    // 4 + (x + 3) → x + 7 (outer Add has const on lhs)
    let vn = reg_vn(0x1000, 8);
    let (mut fg, x) = make_fn_with_var(vn, |b, x| {
        let c3 = b.build_int_const(3u64, NodeOutputType::U64);
        let c4 = b.build_int_const(4u64, NodeOutputType::U64);
        let inner =
            b.build_int_binary_operation(x, c3, IntBinaryOp::Add, NodeOutputType::U64)?;
        b.build_int_binary_operation(c4, inner, IntBinaryOp::Add, NodeOutputType::U64)
    })?;
    let mut changed = true;
    while changed {
        changed = ConstantFold.optimize(&mut fg.graph, fg.entry)?.changed();
    }
    assert_add_with_const(&fg, x, 7, NodeOutputType::U64)?;
    Ok(())
}

#[test]
fn reassoc_chain_three_subs() -> Result<()> {
    // ((x - 4) - 4) - 4 → x - 12.  Requires the fixed-point loop to
    // compose multiple reassociation steps.
    let vn = reg_vn(0x1000, 8);
    let (mut fg, x) = make_fn_with_var(vn, |b, x| {
        let c4 = b.build_int_const(4u64, NodeOutputType::U64);
        let a = b.build_int_binary_operation(x, c4, IntBinaryOp::Sub, NodeOutputType::U64)?;
        let b_ = b.build_int_binary_operation(a, c4, IntBinaryOp::Sub, NodeOutputType::U64)?;
        b.build_int_binary_operation(b_, c4, IntBinaryOp::Sub, NodeOutputType::U64)
    })?;
    let mut changed = true;
    while changed {
        changed = ConstantFold.optimize(&mut fg.graph, fg.entry)?.changed();
    }
    assert_sub_with_const(&fg, x, 12, NodeOutputType::U64)?;
    Ok(())
}

#[test]
fn reassoc_chain_three_subs_u32() -> Result<()> {
    // Same chain but at U32: ((x - 4) - 4) - 4 → x - 12.
    let vn = reg_vn(0x1000, 4);
    let (mut fg, x) = make_fn_with_var(vn, |b, x| {
        let c4 = b.build_int_const(4u64, NodeOutputType::U32);
        let a = b.build_int_binary_operation(x, c4, IntBinaryOp::Sub, NodeOutputType::U32)?;
        let b_ = b.build_int_binary_operation(a, c4, IntBinaryOp::Sub, NodeOutputType::U32)?;
        b.build_int_binary_operation(b_, c4, IntBinaryOp::Sub, NodeOutputType::U32)
    })?;
    let mut changed = true;
    while changed {
        changed = ConstantFold.optimize(&mut fg.graph, fg.entry)?.changed();
    }
    assert_sub_with_const(&fg, x, 12, NodeOutputType::U32)?;
    Ok(())
}

#[test]
fn reassoc_no_fold_without_const() -> Result<()> {
    // (x + y) + z, no constants → untouched.
    let xv = reg_vn(0x1000, 8);
    let yv = reg_vn(0x1008, 8);
    let zv = reg_vn(0x1010, 8);
    let mut b = FunctionBuilder::new_raw(vec![xv, yv, zv], &[xv, yv, zv], &[], &[], None, 0)?;
    let r = b.create_region()?;
    b.set_entry_region(r)?;
    b.set_region(r);
    let x = b.read_variable(&xv)?;
    let y = b.read_variable(&yv)?;
    let z = b.read_variable(&zv)?;
    let inner = b.build_int_binary_operation(x, y, IntBinaryOp::Add, NodeOutputType::U64)?;
    let outer =
        b.build_int_binary_operation(inner, z, IntBinaryOp::Add, NodeOutputType::U64)?;
    b.build_return(Some(outer), &[])?;
    let mut fg = b.build()?;
    let before = return_value(&fg)?;
    // Should not change: no constants anywhere.
    let res = ConstantFold.optimize(&mut fg.graph, fg.entry)?;
    assert!(!res.changed(), "no-const chain should not reassociate");
    assert_eq!(return_value(&fg)?, before);
    Ok(())
}

#[test]
fn distribution_rewrite() -> Result<()> {
    // Build ((a & 0xF0) | (b & 0x0F)) & 0xFF.
    // Rule fires: (a & (0xF0 & 0xFF)) | (b & (0x0F & 0xFF))
    //           = (a & 0xF0) | (b & 0x0F)  — changed=true.
    let av = reg_vn(0x1000, 8);
    let bv = reg_vn(0x1008, 8);
    let mut b = FunctionBuilder::new_raw(vec![av, bv], &[av, bv], &[], &[], None, 0)?;
    let r = b.create_region()?;
    b.set_entry_region(r)?;
    b.set_region(r);
    let a = b.read_variable(&av)?;
    let bval = b.read_variable(&bv)?;
    let f0 = b.build_int_const(0xF0u64, NodeOutputType::U64);
    let f0_ = b.build_int_const(0x0Fu64, NodeOutputType::U64);
    let ff = b.build_int_const(0xFFu64, NodeOutputType::U64);
    let a_and_f0 =
        b.build_int_binary_operation(a, f0, IntBinaryOp::And, NodeOutputType::U64)?;
    let b_and_0f =
        b.build_int_binary_operation(bval, f0_, IntBinaryOp::And, NodeOutputType::U64)?;
    let or_node =
        b.build_int_binary_operation(a_and_f0, b_and_0f, IntBinaryOp::Or, NodeOutputType::U64)?;
    let outer =
        b.build_int_binary_operation(or_node, ff, IntBinaryOp::And, NodeOutputType::U64)?;
    b.build_return(Some(outer), &[])?;
    let mut fg = b.build()?;
    let changed = ConstantFold.optimize(&mut fg.graph, fg.entry)?.changed();
    assert!(changed, "distribution rule should fire");
    Ok(())
}

// ── truncate / extend ─────────────────────────────────────────────────────

#[test]
fn fold_truncate_const() -> Result<()> {
    // The builder's truncate_if_needed already constant-folds inline, so
    // by the time the graph is built there is no Truncate node — just an
    // IntConst with the (possibly unmasked) raw value.
    // Verify that the return value is semantically 0x00 (0xFF00 & 0xFF).
    let fg = make_fn(|b| {
        let wide = b.build_int_const(0xFF00u64, NodeOutputType::U16);
        b.truncate_if_needed(wide, NodeOutputType::U8)
    })?;
    let val = return_value(&fg)?;
    // Use int_const_val which masks to the declared type.
    let semantic = fg.int_const_val(val);
    assert_eq!(semantic, Some(0), "0xFF00 truncated to U8 should be 0");
    // No Truncate nodes should exist.
    assert!(
        !fg.all_node_ids()
            .any(|n| matches!(fg.graph.node_kind(n), NodeKind::Truncate)),
        "builder should have folded the truncate"
    );
    Ok(())
}

/// Rule 4 (`Truncate(IntConst(v)) => int_const(v, ty)`) must mask the
/// stored value to the truncate's output width. Otherwise the IR-layer
/// invariant "an IntConst's stored value fits its declared type" silently
/// breaks: a `Truncate(IntConst(0xFFFF, U16))` would rewrite to
/// `IntConst(0xFFFF, U8)` (typed-narrow but value-wide).
///
/// We can't directly emit `Truncate(IntConst)` because the builder's
/// `truncate_if_needed` short-circuits constants. Instead we feed the
/// truncate from a non-const expression that constant-folds *during* the
/// optimizer's fixed-point loop: `(0xFFFF | 0xFFFF) → IntConst(0xFFFF)`,
/// which then arrives at the still-extant Truncate node and triggers
/// rule 4.
#[test]
fn truncate_int_const_emits_masked_value() -> Result<()> {
    let mut fg = make_fn(|b| {
        let a = b.build_int_const(0xFFFFu64, NodeOutputType::U16);
        let b_ = b.build_int_const(0xFFFFu64, NodeOutputType::U16);
        // Non-const node so truncate_if_needed emits a real Truncate node.
        let or = b.build_int_binary_operation(a, b_, IntBinaryOp::Or, NodeOutputType::U16)?;
        b.truncate_if_needed(or, NodeOutputType::U8)
    })?;
    // Sanity: builder did emit a Truncate node.
    assert!(
        fg.all_node_ids()
            .any(|n| matches!(fg.graph.node_kind(n), NodeKind::Truncate)),
        "test setup expects a Truncate node before optimization",
    );

    assert!(ConstantFold.optimize(&mut fg.graph, fg.entry)?.changed());

    // After optimization the Return's value must be an `IntConst(0xFF)`,
    // i.e. the low byte of 0xFFFF — *masked* to U8. A pre-fix run would
    // store `0xFFFF` (the wider raw value) here.
    let val = return_value(&fg)?;
    let kind = *fg.graph.kind_of_output(val);
    let raw = match kind {
        NodeKind::IntConst(v) => v,
        other => panic!("expected IntConst producer for Return value, got {other:?}"),
    };
    assert_eq!(
        raw & 0xFF,
        raw,
        "Truncate of IntConst must store the masked value, got 0x{raw:X}",
    );
    assert_eq!(raw, 0xFF, "expected low byte 0xFF, got 0x{raw:X}");
    Ok(())
}

// ── BUG-19 helper: Truncate(Extend(x)) round-trip ────────────────────────
//
// Register-merge chains in `write_reg_vn` produce
//   Extend_zext(Truncate(Or(...)))
// and similar `Truncate(Extend(x))` round-trips when the inner expression's
// width equals the outer truncate's output width.  The new round-trip
// rules in `apply_bitcast_extend_rules` collapse these to the inner
// expression.  These do NOT fully fix BUG-19's pattern-matcher walk
// (which still fails on x86 IMUL chains because the surrounding shape
// has Extend/Truncate at the *outer* level — opposite direction), but
// they ARE valid algebraic identities that simplify the IR generally.

use ir::ExtendOp;

/// `Truncate_<W>(ZeroExtend_<W'>(x_<W>))` where `x` already has type `W`
/// must collapse to `x` — the extend added zero bits that the truncate
/// cuts off, so the round-trip is identity.
#[test]
fn fold_truncate_of_zero_extend_round_trip() -> Result<()> {
    let mut fg = make_fn(|b| {
        // Non-const U32 expression so the builder can't short-circuit.
        let a = b.build_int_const(0xAAu64, NodeOutputType::U32);
        let bb = b.build_int_const(0x55u64, NodeOutputType::U32);
        let or = b.build_int_binary_operation(a, bb, IntBinaryOp::Or, NodeOutputType::U32)?;
        let widened = b.extend_if_needed(or, NodeOutputType::U64, ExtendOp::ZeroExtend)?;
        b.truncate_if_needed(widened, NodeOutputType::U32)
    })?;
    let mut changed = true;
    while changed {
        changed = ConstantFold.optimize(&mut fg.graph, fg.entry)?.changed();
    }
    // After optimization the Or's two const inputs fold to IntConst(0xFF),
    // and the Truncate(Extend(IntConst(0xFF))) collapses to IntConst(0xFF).
    // Most importantly: no Truncate or Extend node remains in the chain.
    let val = return_value(&fg)?;
    assert!(
        matches!(fg.graph.kind_of_output(val), NodeKind::IntConst(_)),
        "round-trip + const-fold must leave an IntConst at the root, got {:?}",
        fg.graph.kind_of_output(val)
    );
    // Belt-and-suspenders: walk all reachable nodes and verify no
    // Truncate/Extend survives the chain to the Return.
    for nid in fg.preorder() {
        let kind = fg.graph.node_kind(nid);
        assert!(
            !matches!(kind, NodeKind::Truncate | NodeKind::Extend(_)),
            "Truncate/Extend round-trip must be folded away; found {kind:?}"
        );
    }
    Ok(())
}

/// Same identity holds for `SignExtend`: the sign bits added by the
/// extend are cut off by the truncate.
#[test]
fn fold_truncate_of_sign_extend_round_trip() -> Result<()> {
    let mut fg = make_fn(|b| {
        // Use a non-const Or so the rule fires through the inner expression
        // rather than via direct constant folding.
        let a = b.build_int_const(0x80u64, NodeOutputType::U32);
        let bb = b.build_int_const(0x01u64, NodeOutputType::U32);
        let or = b.build_int_binary_operation(a, bb, IntBinaryOp::Or, NodeOutputType::U32)?;
        let widened = b.extend_if_needed(or, NodeOutputType::U64, ExtendOp::SignExtend)?;
        b.truncate_if_needed(widened, NodeOutputType::U32)
    })?;
    let mut changed = true;
    while changed {
        changed = ConstantFold.optimize(&mut fg.graph, fg.entry)?.changed();
    }
    for nid in fg.preorder() {
        let kind = fg.graph.node_kind(nid);
        assert!(
            !matches!(kind, NodeKind::Truncate | NodeKind::Extend(_)),
            "SignExtend round-trip must be folded away; found {kind:?}"
        );
    }
    Ok(())
}

/// `Truncate_<W>(Mul(SignExt_<W→W'>(a_<W>), SignExt_<W→W'>(b_<W>)))`
/// → `Mul_<W>(a, b)` — narrowing through Mul preserves the lower W bits.
/// MIPS32 lifts `mul a, b` (32×32→64 IntMul) followed by a 32-bit
/// Truncate to recover the integer-register width; without this rule
/// the pattern matcher's data-flow walk for `add(mul(_,_), _)` can't
/// see the Mul through the surrounding Truncate.
#[test]
fn fold_narrow_mul_through_sign_extend() -> Result<()> {
    let mut fg = make_fn(|b| {
        let lhs = b.build_int_const(3u64, NodeOutputType::U32);
        let rhs = b.build_int_const(7u64, NodeOutputType::U32);
        // Use non-const expressions so the constant folder doesn't
        // collapse before our rule runs.
        let lhs_or = b.build_int_binary_operation(lhs, lhs, IntBinaryOp::Or, NodeOutputType::U32)?;
        let rhs_or = b.build_int_binary_operation(rhs, rhs, IntBinaryOp::Or, NodeOutputType::U32)?;
        let lhs_ext = b.extend_if_needed(lhs_or, NodeOutputType::U64, ExtendOp::SignExtend)?;
        let rhs_ext = b.extend_if_needed(rhs_or, NodeOutputType::U64, ExtendOp::SignExtend)?;
        let mul = b.build_int_binary_operation(lhs_ext, rhs_ext, IntBinaryOp::Mul, NodeOutputType::U64)?;
        b.truncate_if_needed(mul, NodeOutputType::U32)
    })?;
    let mut changed = true;
    while changed {
        changed = ConstantFold.optimize(&mut fg.graph, fg.entry)?.changed();
    }
    // After narrowing-through-Mul + constant fold: 3 * 7 = 21 at U32.
    assert_eq!(return_kind(&fg)?, NodeKind::IntConst(21));
    // Nothing wider than U32 should survive (no SignExtend/Mul@U64/Truncate).
    for nid in fg.preorder() {
        let kind = fg.graph.node_kind(nid);
        assert!(
            !matches!(kind, NodeKind::Extend(ExtendOp::SignExtend) | NodeKind::Truncate),
            "narrowing rule must collapse the SignExt-Mul-Truncate chain; \
             found {kind:?}"
        );
    }
    Ok(())
}

/// `Truncate_<W>(Or(any, And(high_mask, _)))` → `Truncate_<W>(any)` —
/// the high-mask half contributes nothing to the lower W bits, so
/// dropping it doesn't change the truncated value.  This is the x86
/// `mov $eax, ...` register-merge cleanup.
#[test]
fn fold_drop_high_half_in_or_truncate() -> Result<()> {
    let mut fg = make_fn(|b| {
        // Build the merge shape: Or(low_part, And(high_mask, junk)).
        let low_part = b.build_int_const(0xAAu64, NodeOutputType::U64);
        let junk = b.build_int_const(0x12345678_DEADBEEFu64, NodeOutputType::U64);
        // Make low_part non-const via Or so the rule fires through it.
        let low_or = b.build_int_binary_operation(
            low_part, low_part, IntBinaryOp::Or, NodeOutputType::U64)?;
        // High mask = 0xFFFF_FFFF_0000_0000 (low 32 bits are zero).
        let high_mask = b.build_int_const(0xFFFFFFFF_00000000u64, NodeOutputType::U64);
        let high_part = b.build_int_binary_operation(
            high_mask, junk, IntBinaryOp::And, NodeOutputType::U64)?;
        let merged = b.build_int_binary_operation(
            low_or, high_part, IntBinaryOp::Or, NodeOutputType::U64)?;
        b.truncate_if_needed(merged, NodeOutputType::U32)
    })?;
    let mut changed = true;
    while changed {
        changed = ConstantFold.optimize(&mut fg.graph, fg.entry)?.changed();
    }
    // After dropping the high half + folding 0xAA | 0xAA = 0xAA at U32:
    // the result is IntConst(0xAA).  No Or remains.
    assert_eq!(return_kind(&fg)?, NodeKind::IntConst(0xAA));
    for nid in fg.preorder() {
        let kind = fg.graph.node_kind(nid);
        assert!(
            !matches!(kind, NodeKind::IntBinaryOp(IntBinaryOp::Or)),
            "high-mask half drop must collapse the Or; found {kind:?}"
        );
    }
    Ok(())
}

/// `Truncate_<W>(And(low_W_mask, x)) → Truncate_<W>(x)` — the AND's
/// effect of zeroing all bits above W is redundant when the truncate
/// is going to discard those bits anyway.
#[test]
fn fold_drop_low_mask_under_truncate() -> Result<()> {
    let mut fg = make_fn(|b| {
        // x is a non-const U64 expression.
        let a = b.build_int_const(0x1234_5678_DEAD_BEEFu64, NodeOutputType::U64);
        let x = b.build_int_binary_operation(a, a, IntBinaryOp::Or, NodeOutputType::U64)?;
        let low_mask = b.build_int_const(0xFFFFFFFFu64, NodeOutputType::U64);
        let masked = b.build_int_binary_operation(
            low_mask, x, IntBinaryOp::And, NodeOutputType::U64)?;
        b.truncate_if_needed(masked, NodeOutputType::U32)
    })?;
    let mut changed = true;
    while changed {
        changed = ConstantFold.optimize(&mut fg.graph, fg.entry)?.changed();
    }
    // After dropping the redundant And + folding the OR-of-itself:
    // result is IntConst(0xDEADBEEF) at U32.
    assert_eq!(return_kind(&fg)?, NodeKind::IntConst(0xDEADBEEF));
    Ok(())
}

/// The round-trip rule must NOT fire when `x`'s type is *narrower* than
/// the truncate's output type — that's a real width-narrowing operation,
/// not an identity.  `Truncate_U16(Extend_U64(x_U32))` is still a real
/// truncation from U32 to U16.
#[test]
fn fold_truncate_of_extend_skips_when_widths_differ() -> Result<()> {
    let mut fg = make_fn(|b| {
        let a = b.build_int_const(0xAAu64, NodeOutputType::U32);
        let bb = b.build_int_const(0x55u64, NodeOutputType::U32);
        let or = b.build_int_binary_operation(a, bb, IntBinaryOp::Or, NodeOutputType::U32)?;
        let widened = b.extend_if_needed(or, NodeOutputType::U64, ExtendOp::ZeroExtend)?;
        // Truncate to U16 — narrower than the inner Or's U32 width, so the
        // round-trip rule must NOT fire.  Constant-fold can still collapse
        // the const-Or, but the truncate must remain (or its result must
        // still semantically be U16).
        b.truncate_if_needed(widened, NodeOutputType::U16)
    })?;
    let mut changed = true;
    while changed {
        changed = ConstantFold.optimize(&mut fg.graph, fg.entry)?.changed();
    }
    // The result must be U16-typed.
    let val = return_value(&fg)?;
    assert_eq!(
        fg.graph.output_kind(val),
        ir::node::NodeOutputKind::OutputType(NodeOutputType::U16),
        "Truncate_U16(Extend_U64(U32)) must keep U16 typing — round-trip \
         rule must not fire when inner width != outer truncate width"
    );
    Ok(())
}

// ── boolean folding ───────────────────────────────────────────────────────

#[test]
fn fold_bool_neg_const() -> Result<()> {
    let mut fg = make_fn(|b| {
        let t = b.build_boolean_const(true);
        b.build_boolean_unary_operation(t, BoolUnaryOp::Neg)
    })?;
    assert!(ConstantFold.optimize(&mut fg.graph, fg.entry)?.changed());
    assert_eq!(return_kind(&fg)?, NodeKind::BoolConst(false));
    Ok(())
}

#[test]
fn fold_bool_and_consts() -> Result<()> {
    let mut fg = make_fn(|b| {
        let t = b.build_boolean_const(true);
        let f = b.build_boolean_const(false);
        b.build_boolean_operation(t, f, BoolBinaryOp::And)
    })?;
    assert!(ConstantFold.optimize(&mut fg.graph, fg.entry)?.changed());
    assert_eq!(return_kind(&fg)?, NodeKind::BoolConst(false));
    Ok(())
}

// ── no-fold edge cases ────────────────────────────────────────────────────

#[test]
fn no_fold_div_by_zero() -> Result<()> {
    let mut fg = make_fn(|b| {
        let x = b.build_int_const(10u64, NodeOutputType::U64);
        let zero = b.build_int_const(0u64, NodeOutputType::U64);
        b.build_int_binary_operation(x, zero, IntBinaryOp::Div, NodeOutputType::U64)
    })?;
    // Should not fold (division by zero is undefined).
    assert!(!ConstantFold.optimize(&mut fg.graph, fg.entry)?.changed());
    assert!(matches!(
        return_kind(&fg)?,
        NodeKind::IntBinaryOp(IntBinaryOp::Div)
    ));
    Ok(())
}

#[test]
fn fold_int_cmp_equal_consts() -> Result<()> {
    let mut fg = make_fn(|b| {
        let c5 = b.build_int_const(5u64, NodeOutputType::U64);
        let c5b = b.build_int_const(5u64, NodeOutputType::U64);
        b.build_int_cmp_operation(c5, c5b, IntCmpOp::Equal, NodeOutputType::U64)
    })?;
    assert!(ConstantFold.optimize(&mut fg.graph, fg.entry)?.changed());
    assert_eq!(return_kind(&fg)?, NodeKind::BoolConst(true));
    Ok(())
}

#[test]
fn fold_int_cmp_less_consts() -> Result<()> {
    let mut fg = make_fn(|b| {
        let c3 = b.build_int_const(3u64, NodeOutputType::U64);
        let c5 = b.build_int_const(5u64, NodeOutputType::U64);
        b.build_int_cmp_operation(c3, c5, IntCmpOp::Less, NodeOutputType::U64)
    })?;
    assert!(ConstantFold.optimize(&mut fg.graph, fg.entry)?.changed());
    assert_eq!(return_kind(&fg)?, NodeKind::BoolConst(true));
    Ok(())
}

// ── Popcount / Lzcount ────────────────────────────────────────────────────

#[test]
fn fold_popcount_const() -> Result<()> {
    // popcount(0b10110101) = 5
    let mut fg = make_fn(|b| {
        let v = b.build_int_const(0b10110101u64, NodeOutputType::U8);
        b.build_popcount(v, NodeOutputType::U8)
    })?;
    assert!(ConstantFold.optimize(&mut fg.graph, fg.entry)?.changed());
    assert_eq!(return_kind(&fg)?, NodeKind::IntConst(5));
    Ok(())
}

#[test]
fn fold_popcount_zero() -> Result<()> {
    let mut fg = make_fn(|b| {
        let v = b.build_int_const(0u64, NodeOutputType::U64);
        b.build_popcount(v, NodeOutputType::U64)
    })?;
    assert!(ConstantFold.optimize(&mut fg.graph, fg.entry)?.changed());
    assert_eq!(return_kind(&fg)?, NodeKind::IntConst(0));
    Ok(())
}

#[test]
fn fold_lzcount_msb_set() -> Result<()> {
    // lzcount(0x80u8) = 0 (MSB is set)
    let mut fg = make_fn(|b| {
        let v = b.build_int_const(0x80u64, NodeOutputType::U8);
        b.build_lzcount(v, NodeOutputType::U8)
    })?;
    assert!(ConstantFold.optimize(&mut fg.graph, fg.entry)?.changed());
    assert_eq!(return_kind(&fg)?, NodeKind::IntConst(0));
    Ok(())
}

#[test]
fn fold_lzcount_one() -> Result<()> {
    // lzcount(1u8) = 7 (only bit 0 set in an 8-bit value)
    let mut fg = make_fn(|b| {
        let v = b.build_int_const(1u64, NodeOutputType::U8);
        b.build_lzcount(v, NodeOutputType::U8)
    })?;
    assert!(ConstantFold.optimize(&mut fg.graph, fg.entry)?.changed());
    assert_eq!(return_kind(&fg)?, NodeKind::IntConst(7));
    Ok(())
}

#[test]
fn fold_lzcount_zero_u32() -> Result<()> {
    // lzcount(0_U32) must fold to 32 (the type's bit width). The previous
    // formula `(masked << (64 - bits)).leading_zeros()` returned 64 when
    // masked was 0, ignoring the type's narrower width.
    let mut fg = make_fn(|b| {
        let v = b.build_int_const(0u64, NodeOutputType::U32);
        b.build_lzcount(v, NodeOutputType::U32)
    })?;
    assert!(ConstantFold.optimize(&mut fg.graph, fg.entry)?.changed());
    assert_eq!(return_kind(&fg)?, NodeKind::IntConst(32));
    Ok(())
}

#[test]
fn fold_lzcount_zero_u8() -> Result<()> {
    let mut fg = make_fn(|b| {
        let v = b.build_int_const(0u64, NodeOutputType::U8);
        b.build_lzcount(v, NodeOutputType::U8)
    })?;
    assert!(ConstantFold.optimize(&mut fg.graph, fg.entry)?.changed());
    assert_eq!(return_kind(&fg)?, NodeKind::IntConst(8));
    Ok(())
}

#[test]
fn fold_lzcount_zero_u64() -> Result<()> {
    // U64 happened to work on the unfixed code (64 - 64 = 0 shift), but pin
    // it with a regression test so the fix doesn't break it.
    let mut fg = make_fn(|b| {
        let v = b.build_int_const(0u64, NodeOutputType::U64);
        b.build_lzcount(v, NodeOutputType::U64)
    })?;
    assert!(ConstantFold.optimize(&mut fg.graph, fg.entry)?.changed());
    assert_eq!(return_kind(&fg)?, NodeKind::IntConst(64));
    Ok(())
}

/// Builds a function whose Return value is `kind(wide_const)`, where the
/// wide constant has declared type `wide_ty` (U128 or U256) and `kind` is
/// either `NodeKind::Lzcount` or `NodeKind::Popcount`.
///
/// `FunctionBuilder::build_int_const` panics on U128/U256 by design, and
/// `build_lzcount`/`build_popcount` would coerce through `convert_to_int_if_needed`
/// (truncating the input to U64). So we build the placeholder skeleton with
/// a U64 const + return, then graft the wide const + unary node directly via
/// the `BuiltFunctionGraph` / `Graph` mutators and rewire the Return.
fn build_unary_with_wide_const_input(
    kind: NodeKind,
    wide_ty: NodeOutputType,
    out_ty: NodeOutputType,
) -> Result<ir::BuiltFunctionGraph> {
    use ir::node::NodeOutputKind;
    let mut fg = make_fn(|b| Ok(b.build_int_const(0u64, NodeOutputType::U64)))?;
    let placeholder = return_value(&fg)?;
    let wide_const = fg.make_int_const(0xFF, wide_ty)?;
    let unary_node = fg.graph.create_node(
        kind,
        [wide_const],
        [NodeOutputKind::OutputType(out_ty)],
    );
    let unary_out = fg.graph.node_outputs_exact::<1>(unary_node)?[0];
    fg.replace_all_uses(placeholder, unary_out)?;
    Ok(fg)
}

#[test]
fn fold_lzcount_u128_input_skips_cleanly() -> Result<()> {
    // Lzcount on a U128 IntConst must not propagate ExpectedIntegerType — the
    // fold can't compute leading-zeros for a width that doesn't fit u64, so
    // the rule should silently skip (Error::skip) rather than crash the
    // whole optimizer pipeline.
    let mut fg = build_unary_with_wide_const_input(
        NodeKind::Lzcount,
        NodeOutputType::U128,
        NodeOutputType::U64,
    )?;
    let result = ConstantFold.optimize(&mut fg.graph, fg.entry);
    assert!(
        result.is_ok(),
        "ConstantFold must not error on Lzcount(U128 const), got {:?}",
        result.err(),
    );
    Ok(())
}

#[test]
fn fold_lzcount_u256_input_skips_cleanly() -> Result<()> {
    let mut fg = build_unary_with_wide_const_input(
        NodeKind::Lzcount,
        NodeOutputType::U256,
        NodeOutputType::U64,
    )?;
    let result = ConstantFold.optimize(&mut fg.graph, fg.entry);
    assert!(
        result.is_ok(),
        "ConstantFold must not error on Lzcount(U256 const), got {:?}",
        result.err(),
    );
    Ok(())
}

#[test]
fn fold_popcount_u128_input_skips_cleanly() -> Result<()> {
    // Popcount on a U128 IntConst has the same shape: the masking step
    // (get_unsigned_int(U128, _) == None) must trigger a skip, not propagate
    // ExpectedIntegerType up through the pipeline.
    let mut fg = build_unary_with_wide_const_input(
        NodeKind::Popcount,
        NodeOutputType::U128,
        NodeOutputType::U64,
    )?;
    let result = ConstantFold.optimize(&mut fg.graph, fg.entry);
    assert!(
        result.is_ok(),
        "ConstantFold must not error on Popcount(U128 const), got {:?}",
        result.err(),
    );
    Ok(())
}

#[test]
fn fold_popcount_u256_input_skips_cleanly() -> Result<()> {
    let mut fg = build_unary_with_wide_const_input(
        NodeKind::Popcount,
        NodeOutputType::U256,
        NodeOutputType::U64,
    )?;
    let result = ConstantFold.optimize(&mut fg.graph, fg.entry);
    assert!(
        result.is_ok(),
        "ConstantFold must not error on Popcount(U256 const), got {:?}",
        result.err(),
    );
    Ok(())
}

// ── Float constant folding ────────────────────────────────────────────────

#[test]
fn fold_f32_add_consts() -> Result<()> {
    let mut fg = make_fn(|b| {
        let a = b.build_float_const(3.0f32.to_bits() as u64, NodeOutputType::F32);
        let c = b.build_float_const(4.0f32.to_bits() as u64, NodeOutputType::F32);
        b.build_float_binary_op(a, c, FloatBinaryOp::Add, NodeOutputType::F32)
    })?;
    assert!(ConstantFold.optimize(&mut fg.graph, fg.entry)?.changed());
    assert_eq!(
        return_kind(&fg)?,
        NodeKind::FloatConst(7.0f32.to_bits() as u64)
    );
    Ok(())
}

#[test]
fn fold_f32_mul_consts() -> Result<()> {
    let mut fg = make_fn(|b| {
        let a = b.build_float_const(3.0f32.to_bits() as u64, NodeOutputType::F32);
        let c = b.build_float_const(4.0f32.to_bits() as u64, NodeOutputType::F32);
        b.build_float_binary_op(a, c, FloatBinaryOp::Mul, NodeOutputType::F32)
    })?;
    assert!(ConstantFold.optimize(&mut fg.graph, fg.entry)?.changed());
    assert_eq!(
        return_kind(&fg)?,
        NodeKind::FloatConst(12.0f32.to_bits() as u64)
    );
    Ok(())
}

#[test]
fn fold_f32_div_consts() -> Result<()> {
    let mut fg = make_fn(|b| {
        let a = b.build_float_const(10.0f32.to_bits() as u64, NodeOutputType::F32);
        let c = b.build_float_const(4.0f32.to_bits() as u64, NodeOutputType::F32);
        b.build_float_binary_op(a, c, FloatBinaryOp::Div, NodeOutputType::F32)
    })?;
    assert!(ConstantFold.optimize(&mut fg.graph, fg.entry)?.changed());
    assert_eq!(
        return_kind(&fg)?,
        NodeKind::FloatConst(2.5f32.to_bits() as u64)
    );
    Ok(())
}

#[test]
fn fold_f64_add_consts() -> Result<()> {
    let mut fg = make_fn(|b| {
        let a = b.build_float_const(3.0f64.to_bits(), NodeOutputType::F64);
        let c = b.build_float_const(4.0f64.to_bits(), NodeOutputType::F64);
        b.build_float_binary_op(a, c, FloatBinaryOp::Add, NodeOutputType::F64)
    })?;
    assert!(ConstantFold.optimize(&mut fg.graph, fg.entry)?.changed());
    assert_eq!(return_kind(&fg)?, NodeKind::FloatConst(7.0f64.to_bits()));
    Ok(())
}

#[test]
fn fold_f64_mul_consts() -> Result<()> {
    let mut fg = make_fn(|b| {
        let a = b.build_float_const(3.0f64.to_bits(), NodeOutputType::F64);
        let c = b.build_float_const(4.0f64.to_bits(), NodeOutputType::F64);
        b.build_float_binary_op(a, c, FloatBinaryOp::Mul, NodeOutputType::F64)
    })?;
    assert!(ConstantFold.optimize(&mut fg.graph, fg.entry)?.changed());
    assert_eq!(return_kind(&fg)?, NodeKind::FloatConst(12.0f64.to_bits()));
    Ok(())
}

#[test]
fn fold_f64_div_consts() -> Result<()> {
    let mut fg = make_fn(|b| {
        let a = b.build_float_const(10.0f64.to_bits(), NodeOutputType::F64);
        let c = b.build_float_const(4.0f64.to_bits(), NodeOutputType::F64);
        b.build_float_binary_op(a, c, FloatBinaryOp::Div, NodeOutputType::F64)
    })?;
    assert!(ConstantFold.optimize(&mut fg.graph, fg.entry)?.changed());
    assert_eq!(return_kind(&fg)?, NodeKind::FloatConst(2.5f64.to_bits()));
    Ok(())
}

#[test]
fn fold_f32_less_true() -> Result<()> {
    let mut fg = make_fn(|b| {
        let a = b.build_float_const(3.0f32.to_bits() as u64, NodeOutputType::F32);
        let c = b.build_float_const(4.0f32.to_bits() as u64, NodeOutputType::F32);
        b.build_float_cmp_op(a, c, FloatCmpOp::Less)
    })?;
    assert!(ConstantFold.optimize(&mut fg.graph, fg.entry)?.changed());
    assert_eq!(return_kind(&fg)?, NodeKind::BoolConst(true));
    Ok(())
}

#[test]
fn fold_f64_equal_true() -> Result<()> {
    let mut fg = make_fn(|b| {
        let a = b.build_float_const(4.0f64.to_bits(), NodeOutputType::F64);
        let c = b.build_float_const(4.0f64.to_bits(), NodeOutputType::F64);
        b.build_float_cmp_op(a, c, FloatCmpOp::Equal)
    })?;
    assert!(ConstantFold.optimize(&mut fg.graph, fg.entry)?.changed());
    assert_eq!(return_kind(&fg)?, NodeKind::BoolConst(true));
    Ok(())
}

#[test]
fn fold_f64_equal_nan_false() -> Result<()> {
    // NaN != NaN per IEEE 754
    let nan = f64::NAN.to_bits();
    let mut fg = make_fn(|b| {
        let a = b.build_float_const(nan, NodeOutputType::F64);
        let c = b.build_float_const(nan, NodeOutputType::F64);
        b.build_float_cmp_op(a, c, FloatCmpOp::Equal)
    })?;
    assert!(ConstantFold.optimize(&mut fg.graph, fg.entry)?.changed());
    assert_eq!(return_kind(&fg)?, NodeKind::BoolConst(false));
    Ok(())
}

#[test]
fn fold_f32_neg_const() -> Result<()> {
    let mut fg = make_fn(|b| {
        let v = b.build_float_const(2.0f32.to_bits() as u64, NodeOutputType::F32);
        b.build_float_unary_op(v, FloatUnaryOp::Neg, NodeOutputType::F32)
    })?;
    assert!(ConstantFold.optimize(&mut fg.graph, fg.entry)?.changed());
    assert_eq!(
        return_kind(&fg)?,
        NodeKind::FloatConst((-2.0f32).to_bits() as u64)
    );
    Ok(())
}

#[test]
fn fold_f64_abs_const() -> Result<()> {
    let mut fg = make_fn(|b| {
        let v = b.build_float_const((-3.0f64).to_bits(), NodeOutputType::F64);
        b.build_float_unary_op(v, FloatUnaryOp::Abs, NodeOutputType::F64)
    })?;
    assert!(ConstantFold.optimize(&mut fg.graph, fg.entry)?.changed());
    assert_eq!(return_kind(&fg)?, NodeKind::FloatConst(3.0f64.to_bits()));
    Ok(())
}

#[test]
fn fold_f64_sqrt_const() -> Result<()> {
    let mut fg = make_fn(|b| {
        let v = b.build_float_const(4.0f64.to_bits(), NodeOutputType::F64);
        b.build_float_unary_op(v, FloatUnaryOp::Sqrt, NodeOutputType::F64)
    })?;
    assert!(ConstantFold.optimize(&mut fg.graph, fg.entry)?.changed());
    assert_eq!(return_kind(&fg)?, NodeKind::FloatConst(2.0f64.to_bits()));
    Ok(())
}

#[test]
fn fold_float_mul_by_one_identity() -> Result<()> {
    let mut fg = make_fn(|b| {
        let one = b.build_float_const(1.0f64.to_bits(), NodeOutputType::F64);
        let x = b.build_float_const(2.5f64.to_bits(), NodeOutputType::F64);
        b.build_float_binary_op(x, one, FloatBinaryOp::Mul, NodeOutputType::F64)
    })?;
    assert!(ConstantFold.optimize(&mut fg.graph, fg.entry)?.changed());
    assert_eq!(return_kind(&fg)?, NodeKind::FloatConst(2.5f64.to_bits()));
    Ok(())
}

#[test]
fn fold_float_div_by_one_identity() -> Result<()> {
    let mut fg = make_fn(|b| {
        let one = b.build_float_const(1.0f64.to_bits(), NodeOutputType::F64);
        let x = b.build_float_const(2.5f64.to_bits(), NodeOutputType::F64);
        b.build_float_binary_op(x, one, FloatBinaryOp::Div, NodeOutputType::F64)
    })?;
    assert!(ConstantFold.optimize(&mut fg.graph, fg.entry)?.changed());
    assert_eq!(return_kind(&fg)?, NodeKind::FloatConst(2.5f64.to_bits()));
    Ok(())
}

#[test]
fn fold_f64_round_uses_ties_to_even_not_away_from_zero() -> Result<()> {
    // FloatUnaryOp::Round is documented (op_kinds.rs) as "Round to nearest
    // integer (ties to even)" — the IEEE 754 default rounding mode and what
    // x86/ARM hardware emits. Half-ties must round to the nearest even
    // integer, NOT away from zero.
    let cases: &[(f64, f64)] = &[
        (0.5, 0.0),
        (1.5, 2.0),
        (2.5, 2.0),
        (3.5, 4.0),
        (-0.5, -0.0),
        (-1.5, -2.0),
        (-2.5, -2.0),
    ];
    for &(input, expected) in cases {
        let mut fg = make_fn(|b| {
            let v = b.build_float_const(input.to_bits(), NodeOutputType::F64);
            b.build_float_unary_op(v, FloatUnaryOp::Round, NodeOutputType::F64)
        })?;
        assert!(ConstantFold.optimize(&mut fg.graph, fg.entry)?.changed(),
            "Round({input}) did not fold");
        assert_eq!(
            return_kind(&fg)?,
            NodeKind::FloatConst(expected.to_bits()),
            "Round({input}) folded to wrong value (expected {expected})",
        );
    }
    Ok(())
}

#[test]
fn fold_f32_round_uses_ties_to_even_not_away_from_zero() -> Result<()> {
    let cases: &[(f32, f32)] = &[
        (0.5, 0.0),
        (1.5, 2.0),
        (2.5, 2.0),
        (-0.5, -0.0),
        (-2.5, -2.0),
    ];
    for &(input, expected) in cases {
        let mut fg = make_fn(|b| {
            let v = b.build_float_const(input.to_bits() as u64, NodeOutputType::F32);
            b.build_float_unary_op(v, FloatUnaryOp::Round, NodeOutputType::F32)
        })?;
        assert!(ConstantFold.optimize(&mut fg.graph, fg.entry)?.changed(),
            "Round({input}) did not fold");
        assert_eq!(
            return_kind(&fg)?,
            NodeKind::FloatConst(expected.to_bits() as u64),
            "Round({input}) folded to wrong value (expected {expected})",
        );
    }
    Ok(())
}

#[test]
fn fold_bitcast_identity_int_bits_to_float_of_float_bits_to_int() -> Result<()> {
    // IntBitsToFloat(FloatBitsToInt(FloatAdd(1.0, 2.0)))
    // → first, FloatAdd(1.0, 2.0) folds to FloatConst(3.0)
    // → then,  IntBitsToFloat(FloatBitsToInt(FloatConst(3.0))) simplifies to FloatConst(3.0)
    //   via the bitcast-identity: replace uses of IntBitsToFloat with FloatBitsToInt's input.
    let mut fg = make_fn(|b| {
        let a = b.build_float_const(1.0f64.to_bits(), NodeOutputType::F64);
        let b2 = b.build_float_const(2.0f64.to_bits(), NodeOutputType::F64);
        let sum = b.build_float_binary_op(a, b2, FloatBinaryOp::Add, NodeOutputType::F64)?;
        let as_int = b.build_float_bits_to_int(sum, NodeOutputType::U64)?;
        let back_to_float = b.build_int_bits_to_float(as_int, NodeOutputType::F64)?;
        Ok(back_to_float)
    })?;
    assert!(ConstantFold.optimize(&mut fg.graph, fg.entry)?.changed());
    // Float binary fold: sum → FloatConst(3.0).
    // Bitcast identity fold: IntBitsToFloat(FloatBitsToInt(FloatConst(3.0))) → FloatConst(3.0).
    assert_eq!(return_kind(&fg)?, NodeKind::FloatConst(3.0f64.to_bits()));
    Ok(())
}

// ── CastToFloat lowering tests ────────────────────────────────────────────

#[test]
fn cast_to_float_int_const_folds_to_float_const() -> Result<()> {
    let bits = 1.0f64.to_bits();
    let mut fg = make_fn(|b| {
        let int_val = b.build_int_const(bits, NodeOutputType::U64);
        let cast = b.build_cast_to_float(int_val, NodeOutputType::F64);
        Ok(cast)
    })?;
    assert!(ConstantFold.optimize(&mut fg.graph, fg.entry)?.changed());
    // CastToFloat(IntConst(bits)) → FloatConst(bits)
    assert_eq!(return_kind(&fg)?, NodeKind::FloatConst(bits));
    Ok(())
}

#[test]
fn cast_to_float_same_float_type_eliminates() -> Result<()> {
    let bits = 1.0f32.to_bits() as u64;
    let mut fg = make_fn(|b| {
        let float_val = b.build_float_const(bits, NodeOutputType::F32);
        let cast = b.build_cast_to_float(float_val, NodeOutputType::F32);
        Ok(cast)
    })?;
    assert!(ConstantFold.optimize(&mut fg.graph, fg.entry)?.changed());
    // CastToFloat(F32 → F32) → identity (FloatConst)
    assert_eq!(return_kind(&fg)?, NodeKind::FloatConst(bits));
    Ok(())
}

#[test]
fn cast_to_float_int_non_const_lowers_to_int_bits_to_float() -> Result<()> {
    let mut fg = make_fn(|b| {
        let int_a = b.build_int_const(1u64, NodeOutputType::U32);
        let int_b = b.build_int_const(2u64, NodeOutputType::U32);
        // Non-const int (Add result).
        let sum =
            b.build_int_binary_operation(int_a, int_b, IntBinaryOp::Add, NodeOutputType::U32)?;
        let cast = b.build_cast_to_float(sum, NodeOutputType::F32);
        Ok(cast)
    })?;
    assert!(ConstantFold.optimize(&mut fg.graph, fg.entry)?.changed());
    // Should lower to IntBitsToFloat.
    assert_eq!(return_kind(&fg)?, NodeKind::IntBitsToFloat);
    Ok(())
}

#[test]
fn cast_to_float_cross_precision_lowers_to_float_to_float() -> Result<()> {
    let mut fg = make_fn(|b| {
        let f32_val = b.build_float_const(1.0f32.to_bits() as u64, NodeOutputType::F32);
        let cast = b.build_cast_to_float(f32_val, NodeOutputType::F64);
        Ok(cast)
    })?;
    assert!(ConstantFold.optimize(&mut fg.graph, fg.entry)?.changed());
    // F32 → F64 should lower to FloatToFloat.
    assert_eq!(return_kind(&fg)?, NodeKind::FloatToFloat);
    Ok(())
}

// ── Comprehensive tests added in Task 2.E ─────────────────────────────────────

/// Shift constant evaluation: `1 << 4` for U32 → 0x10.
#[test]
fn fold_shl_const_u32() -> Result<()> {
    let mut fg = make_fn(|b| {
        let x = b.build_int_const(1u64, NodeOutputType::U32);
        let n = b.build_int_const(4u64, NodeOutputType::U32);
        b.build_int_binary_operation(x, n, IntBinaryOp::ShiftLeft, NodeOutputType::U32)
    })?;
    assert!(ConstantFold.optimize(&mut fg.graph, fg.entry)?.changed());
    assert_eq!(return_kind(&fg)?, NodeKind::IntConst(0x10));
    Ok(())
}

/// Shift at width boundary: `1 << 31` for U32 → 0x80000000.
#[test]
fn fold_shl_at_width_boundary_u32() -> Result<()> {
    let mut fg = make_fn(|b| {
        let x = b.build_int_const(1u64, NodeOutputType::U32);
        let n = b.build_int_const(31u64, NodeOutputType::U32);
        b.build_int_binary_operation(x, n, IntBinaryOp::ShiftLeft, NodeOutputType::U32)
    })?;
    assert!(ConstantFold.optimize(&mut fg.graph, fg.entry)?.changed());
    assert_eq!(return_kind(&fg)?, NodeKind::IntConst(0x8000_0000));
    Ok(())
}

/// Shift right: `0x80 >> 7` for U8 → 1.
#[test]
fn fold_shr_const_u8() -> Result<()> {
    let mut fg = make_fn(|b| {
        let x = b.build_int_const(0x80u64, NodeOutputType::U8);
        let n = b.build_int_const(7u64, NodeOutputType::U8);
        b.build_int_binary_operation(x, n, IntBinaryOp::ShiftRight, NodeOutputType::U8)
    })?;
    assert!(ConstantFold.optimize(&mut fg.graph, fg.entry)?.changed());
    assert_eq!(return_kind(&fg)?, NodeKind::IntConst(1));
    Ok(())
}

/// NaN propagates through binary float arithmetic.
#[test]
fn fold_f64_nan_plus_one_stays_nan() -> Result<()> {
    let nan = f64::NAN.to_bits();
    let mut fg = make_fn(|b| {
        let a = b.build_float_const(nan, NodeOutputType::F64);
        let one = b.build_float_const(1.0f64.to_bits(), NodeOutputType::F64);
        b.build_float_binary_op(a, one, FloatBinaryOp::Add, NodeOutputType::F64)
    })?;
    assert!(ConstantFold.optimize(&mut fg.graph, fg.entry)?.changed());
    let val = return_value(&fg)?;
    if let NodeKind::FloatConst(bits) = *fg.graph.kind_of_output(val) {
        assert!(f64::from_bits(bits).is_nan(), "NaN must propagate through Add");
    } else {
        return Err(anyhow!("assertion failed: expected FloatConst result"));
    }
    Ok(())
}

/// `inf - inf` is NaN per IEEE 754.
#[test]
fn fold_f64_inf_minus_inf_is_nan() -> Result<()> {
    let inf = f64::INFINITY.to_bits();
    let mut fg = make_fn(|b| {
        let a = b.build_float_const(inf, NodeOutputType::F64);
        let bb = b.build_float_const(inf, NodeOutputType::F64);
        b.build_float_binary_op(a, bb, FloatBinaryOp::Sub, NodeOutputType::F64)
    })?;
    assert!(ConstantFold.optimize(&mut fg.graph, fg.entry)?.changed());
    let val = return_value(&fg)?;
    if let NodeKind::FloatConst(bits) = *fg.graph.kind_of_output(val) {
        assert!(f64::from_bits(bits).is_nan());
    } else {
        return Err(anyhow!("assertion failed: expected FloatConst result"));
    }
    Ok(())
}

/// Bitcast roundtrip on f32 — `IntBitsToFloat(FloatBitsToInt(non-const))` → non-const.
/// Uses a non-const float (the result of a float Add) so the builder doesn't
/// fold the inner cast eagerly.
#[test]
fn fold_bitcast_roundtrip_f32() -> Result<()> {
    let mut fg = make_fn(|b| {
        let a = b.build_float_const(1.0f32.to_bits() as u64, NodeOutputType::F32);
        let b2 = b.build_float_const(1.5f32.to_bits() as u64, NodeOutputType::F32);
        let sum = b.build_float_binary_op(a, b2, FloatBinaryOp::Add, NodeOutputType::F32)?;
        let as_int = b.build_float_bits_to_int(sum, NodeOutputType::U32)?;
        b.build_int_bits_to_float(as_int, NodeOutputType::F32)
    })?;
    assert!(ConstantFold.optimize(&mut fg.graph, fg.entry)?.changed());
    // After folding: float Add → FloatConst(2.5), then bitcast roundtrip
    // collapses to that constant.
    assert_eq!(
        return_kind(&fg)?,
        NodeKind::FloatConst(2.5f32.to_bits() as u64)
    );
    Ok(())
}

/// Single-pass cascade: `((1 + 2) + 3) + 4` must converge to `IntConst(10)`
/// in a single `optimize()` call (i.e. without relying on the outer
/// pipeline fixed-point loop). Verifies that consumers are re-enqueued
/// into the worklist after a rule rewrites a node.
#[test]
fn single_pass_propagates_through_chain() -> Result<()> {
    let mut fg = make_fn(|b| {
        let one = b.build_int_const(1u64, NodeOutputType::U32);
        let two = b.build_int_const(2u64, NodeOutputType::U32);
        let three = b.build_int_const(3u64, NodeOutputType::U32);
        let four = b.build_int_const(4u64, NodeOutputType::U32);
        let c1 = b.build_int_binary_operation(one, two, IntBinaryOp::Add, NodeOutputType::U32)?;
        let c2 = b.build_int_binary_operation(c1, three, IntBinaryOp::Add, NodeOutputType::U32)?;
        b.build_int_binary_operation(c2, four, IntBinaryOp::Add, NodeOutputType::U32)
    })?;

    // Single optimize() call — must converge without the outer pipeline loop.
    ConstantFold.optimize(&mut fg.graph, fg.entry)?;

    assert_eq!(
        return_kind(&fg)?,
        NodeKind::IntConst(10),
        "expected single-pass convergence to IntConst(10)"
    );
    Ok(())
}

/// 10-deep `((((x - 1) - 1) ...) - 1)` chain — must collapse to `x - 10`
/// via the worklist re-enqueueing reassociation rules along the way.
#[test]
fn fold_chain_of_ten_subs_reassociates() -> Result<()> {
    let vn = reg_vn(0x1000, 8);
    let (mut fg, x) = make_fn_with_var(vn, |b, x| {
        let mut acc = x;
        for _ in 0..10 {
            let one = b.build_int_const(1u64, NodeOutputType::U64);
            acc = b.build_int_binary_operation(acc, one, IntBinaryOp::Sub, NodeOutputType::U64)?;
        }
        Ok(acc)
    })?;
    let mut changed = true;
    while changed {
        changed = ConstantFold.optimize(&mut fg.graph, fg.entry)?.changed();
    }
    assert_sub_with_const(&fg, x, 10, NodeOutputType::U64)?;
    Ok(())
}

#[test]
fn sdiv_narrow_int_min_neg_one_skips() {
    use crate::constant_fold::eval_int::eval_int_binary;
    use ir::IntBinaryOp;
    use ir::node::NodeOutputType;

    // i32::MIN as u32, then masked to u64. Same shape as the u64 case
    // already guarded explicitly; should also return None.
    assert_eq!(
        eval_int_binary(IntBinaryOp::Sdiv, 0x8000_0000, 0xFFFF_FFFF, NodeOutputType::U32),
        None,
        "Sdiv(i32::MIN, -1) on U32 must skip — signed overflow"
    );
    // i16::MIN, -1 on U16.
    assert_eq!(
        eval_int_binary(IntBinaryOp::Sdiv, 0x8000, 0xFFFF, NodeOutputType::U16),
        None,
        "Sdiv(i16::MIN, -1) on U16 must skip — signed overflow"
    );
    // i8::MIN, -1 on U8.
    assert_eq!(
        eval_int_binary(IntBinaryOp::Sdiv, 0x80, 0xFF, NodeOutputType::U8),
        None,
        "Sdiv(i8::MIN, -1) on U8 must skip — signed overflow"
    );
}

#[test]
fn srem_narrow_int_min_neg_one_skips() {
    use crate::constant_fold::eval_int::eval_int_binary;
    use ir::IntBinaryOp;
    use ir::node::NodeOutputType;

    // Same INT_MIN/-1 case for Srem on every narrow signed type.
    assert_eq!(
        eval_int_binary(IntBinaryOp::Srem, 0x8000_0000, 0xFFFF_FFFF, NodeOutputType::U32),
        None,
        "Srem(i32::MIN, -1) on U32 must skip"
    );
    assert_eq!(
        eval_int_binary(IntBinaryOp::Srem, 0x8000, 0xFFFF, NodeOutputType::U16),
        None,
    );
    assert_eq!(
        eval_int_binary(IntBinaryOp::Srem, 0x80, 0xFF, NodeOutputType::U8),
        None,
    );
}

#[test]
fn eval_int_binary_unsigned_div_unmasked_u8() {
    use crate::constant_fold::eval_int::eval_int_binary;
    use ir::IntBinaryOp;
    use ir::node::NodeOutputType;

    // U8 Div with l carrying high garbage bits beyond U8.
    // Masked: 0xFF / 2 = 0x7F. Unmasked-eval: 0x1FF / 2 = 0xFF (wrong).
    assert_eq!(
        eval_int_binary(IntBinaryOp::Div, 0x1FF, 2, NodeOutputType::U8),
        Some(0x7F),
        "Div must mask inputs to U8 before division"
    );
}

#[test]
fn eval_int_binary_unsigned_rem_unmasked_u16() {
    use crate::constant_fold::eval_int::eval_int_binary;
    use ir::IntBinaryOp;
    use ir::node::NodeOutputType;

    // Masked: 0xFFFF % 0x10 = 0x0F. Unmasked-eval: 0x1FFFF % 0x10 = 0x0F.
    // Pick a divisor that distinguishes: 0xFFFF % 7 = 1, 0x1FFFF % 7 = 5.
    assert_eq!(
        eval_int_binary(IntBinaryOp::Rem, 0x1FFFF, 7, NodeOutputType::U16),
        Some(1),
        "Rem must mask inputs to U16 before remainder"
    );
}

#[test]
fn eval_int_binary_unsigned_shr_unmasked_u8() {
    use crate::constant_fold::eval_int::eval_int_binary;
    use ir::IntBinaryOp;
    use ir::node::NodeOutputType;

    // Masked: 0xFF >> 1 = 0x7F. Unmasked-eval: 0x1FF >> 1 = 0xFF, masked = 0xFF.
    assert_eq!(
        eval_int_binary(IntBinaryOp::ShiftRight, 0x1FF, 1, NodeOutputType::U8),
        Some(0x7F),
        "ShiftRight must mask the input to U8 before shifting"
    );
}

#[test]
fn eval_int_cmp_equal_unmasked_u8() {
    use crate::constant_fold::eval_int::eval_int_cmp;
    use ir::IntCmpOp;
    use ir::node::NodeOutputType;

    // Masked: 0xFF == 0xFF → true. Unmasked-eval: 0x1FF != 0xFF → false.
    assert!(
        eval_int_cmp(IntCmpOp::Equal, 0x1FF, 0xFF, NodeOutputType::U8).unwrap(),
        "Equal must mask both sides to U8 before comparing"
    );
}

#[test]
fn eval_int_cmp_less_unmasked_u8() {
    use crate::constant_fold::eval_int::eval_int_cmp;
    use ir::IntCmpOp;
    use ir::node::NodeOutputType;

    // Masked: 0x00 < 0x01 → true. Unmasked-eval: 0x100 < 0x01 → false.
    assert!(
        eval_int_cmp(IntCmpOp::Less, 0x100, 0x01, NodeOutputType::U8).unwrap(),
        "Less must mask both sides to U8 before comparing"
    );
}

#[test]
fn eval_int_cmp_carry_unmasked_u8() {
    use crate::constant_fold::eval_int::eval_int_cmp;
    use ir::IntCmpOp;
    use ir::node::NodeOutputType;

    // Masked: 0x00 + 0x00 → no carry. Unmasked-eval: 0x100 + 0 = 0x100 > 0xFF → false-carry.
    assert!(
        !eval_int_cmp(IntCmpOp::Carry, 0x100, 0, NodeOutputType::U8).unwrap(),
        "Carry must mask both sides before checking overflow"
    );
}

#[test]
fn eval_int_cmp_borrow_unmasked_u8() {
    use crate::constant_fold::eval_int::eval_int_cmp;
    use ir::IntCmpOp;
    use ir::node::NodeOutputType;

    // Masked: 0x00 < 0x01 → true. Unmasked-eval: 0x100 < 0x01 → false.
    assert!(
        eval_int_cmp(IntCmpOp::Borrow, 0x100, 0x01, NodeOutputType::U8).unwrap(),
        "Borrow must mask both sides to U8 before comparing"
    );
}

// ── BUG-21 ARM residue: IntUnaryOp::Neg/Not constant-fold semantics ──
//
// The IR's enum variants follow Sleigh's counter-intuitive opcode
// naming, which the analyzer dispatch table propagates:
//   * `IntUnaryOp::Neg` is BITWISE NOT (Sleigh `IntNeg`).
//   * `IntUnaryOp::Not` is TWO'S COMPLEMENT (Sleigh `Int2Comp`).
//
// Pre-fix the constant-fold rules had the two operations swapped:
// `Neg => v.wrapping_neg()` and `Not => !v`.  The MVN-based ARM
// `if_returns_const` lowering produced `IntUnaryOp::Neg(IntConst(49))`
// (= ~49 = -50), but the fold computed `wrapping_neg(49) = -49`,
// off by one — the `has_constant(g, -50)` pattern check failed.

use ir::IntUnaryOp;

/// `IntUnaryOp::Neg` of `IntConst(49)` at U32 must fold to `~49`
/// (= 0xFFFF_FFCE = 4_294_967_246) — bitwise NOT, NOT two's complement.
#[test]
fn fold_int_unary_neg_is_bitwise_not_u32() -> Result<()> {
    let mut fg = make_fn(|b| {
        let c = b.build_int_const(49u64, NodeOutputType::U32);
        b.build_int_unary_operation(c, IntUnaryOp::Neg, NodeOutputType::U32)
    })?;
    assert!(ConstantFold.optimize(&mut fg.graph, fg.entry)?.changed());
    assert_eq!(
        return_kind(&fg)?,
        NodeKind::IntConst(0xFFFF_FFCE),
        "IntUnaryOp::Neg(49) must fold to bitwise NOT (=~49=0xFFFFFFCE), \
         not two's complement (=0xFFFFFFCF=-49)"
    );
    Ok(())
}

/// `IntUnaryOp::Not` of `IntConst(50)` at U32 must fold to `-50`
/// (= 0xFFFF_FFCE = 4_294_967_246) — two's complement, NOT bitwise NOT.
#[test]
fn fold_int_unary_not_is_two_complement_u32() -> Result<()> {
    let mut fg = make_fn(|b| {
        let c = b.build_int_const(50u64, NodeOutputType::U32);
        b.build_int_unary_operation(c, IntUnaryOp::Not, NodeOutputType::U32)
    })?;
    assert!(ConstantFold.optimize(&mut fg.graph, fg.entry)?.changed());
    assert_eq!(
        return_kind(&fg)?,
        NodeKind::IntConst(0xFFFF_FFCE),
        "IntUnaryOp::Not(50) must fold to two's complement (=-50=0xFFFFFFCE), \
         not bitwise NOT (=~50=0xFFFFFFCD)"
    );
    Ok(())
}

/// Round-trip at U8: `Neg(Neg(0xAA)) = 0xAA` (bitwise NOT is its own
/// inverse).  Pre-fix the fold computed `wrapping_neg(wrapping_neg(0xAA))
/// = 0xAA` too — coincidentally correct only because two's complement
/// is also its own inverse.  This test pins the *value* at the
/// intermediate step.
#[test]
fn fold_int_unary_neg_intermediate_is_bitwise_not_u8() -> Result<()> {
    let mut fg = make_fn(|b| {
        let c = b.build_int_const(0xAAu64, NodeOutputType::U8);
        b.build_int_unary_operation(c, IntUnaryOp::Neg, NodeOutputType::U8)
    })?;
    assert!(ConstantFold.optimize(&mut fg.graph, fg.entry)?.changed());
    assert_eq!(
        return_kind(&fg)?,
        NodeKind::IntConst(0x55),
        "Neg(0xAA) at U8 must be ~0xAA = 0x55 (bitwise NOT)"
    );
    Ok(())
}

/// Two's complement of 0 is 0 — even with the swap, this case is
/// invariant.  Included as a sanity check for the U64 path.
#[test]
fn fold_int_unary_not_zero_is_zero() -> Result<()> {
    let mut fg = make_fn(|b| {
        let c = b.build_int_const(0u64, NodeOutputType::U64);
        b.build_int_unary_operation(c, IntUnaryOp::Not, NodeOutputType::U64)
    })?;
    assert!(ConstantFold.optimize(&mut fg.graph, fg.entry)?.changed());
    assert_eq!(return_kind(&fg)?, NodeKind::IntConst(0));
    Ok(())
}

/// Bitwise NOT of 0 is all-ones at the type width.  Pre-fix's swap
/// would have computed `wrapping_neg(0) = 0` here — distinguishing
/// the two operations cleanly.
#[test]
fn fold_int_unary_neg_zero_is_all_ones_u32() -> Result<()> {
    let mut fg = make_fn(|b| {
        let c = b.build_int_const(0u64, NodeOutputType::U32);
        b.build_int_unary_operation(c, IntUnaryOp::Neg, NodeOutputType::U32)
    })?;
    assert!(ConstantFold.optimize(&mut fg.graph, fg.entry)?.changed());
    assert_eq!(
        return_kind(&fg)?,
        NodeKind::IntConst(0xFFFF_FFFF),
        "Neg(0) at U32 must be ~0 = 0xFFFFFFFF (bitwise NOT); pre-fix \
         swapped fold would have produced wrapping_neg(0) = 0"
    );
    Ok(())
}

// ── Sleigh INT_LEFT/INT_RIGHT/INT_SRIGHT out-of-range shift semantics ──
//
// Sleigh's `OpBehaviorIntLeft::evaluateBinary` (sleigh/src/opbehavior.cc:411)
// returns 0 when the shift amount is >= 8*sizeout — i.e. shifting by
// the full bit-width or beyond zeroes the value.  `OpBehaviorIntRight`
// has the same rule, and `OpBehaviorIntSright` returns
// `signbit ? all_ones : 0` for full-width-or-greater shifts.
//
// Pre-fix the constant-fold evaluator computed `shift = (s as u32) % bits`,
// which gives `1 << (32 % 32) = 1 << 0 = 1` for `IntConst(1, U32) << 32`
// — diverging from Sleigh by a factor of `2^bits`.  The mismatch
// surfaces whenever a lifter emits a literal shift at the type's
// bit-width (e.g. an unrolled bit-clear that masks via `(value & ~(1 <<
// k))` for k = bit-width as a degenerate "no-op" loop iteration), or
// whenever `KnownBits` propagates a shift constant that happens to
// land at-or-past bits.

/// Sleigh `INT_LEFT(IntConst(1), IntConst(32))` at sizeout=4 evaluates
/// to 0.  Our fold must agree.
#[test]
fn eval_int_binary_shl_at_bit_width_returns_zero_u32() {
    use crate::constant_fold::eval_int::eval_int_binary;

    assert_eq!(
        eval_int_binary(IntBinaryOp::ShiftLeft, 1, 32, NodeOutputType::U32),
        Some(0),
        "Sleigh: 1u32 << 32 = 0 (`r >= 8*sizeout` returns 0 per opbehavior.cc:411). \
         Pre-fix fold computed `1 << (32 % 32) = 1 << 0 = 1` — diverges from Sleigh."
    );
}

/// Sleigh `INT_LEFT(IntConst(1), IntConst(64))` at sizeout=8 evaluates
/// to 0.  At u64 the wider type doesn't change the rule.
#[test]
fn eval_int_binary_shl_at_bit_width_returns_zero_u64() {
    use crate::constant_fold::eval_int::eval_int_binary;

    assert_eq!(
        eval_int_binary(IntBinaryOp::ShiftLeft, 1, 64, NodeOutputType::U64),
        Some(0),
        "Sleigh: 1u64 << 64 = 0.  Pre-fix fold computed `1 << (64 % 64) = 1`."
    );
}

/// Sleigh `INT_LEFT(IntConst(0xFF), IntConst(40))` at sizeout=4 evaluates
/// to 0 (40 > 32).  Beyond-bit-width shifts also zero the result.
#[test]
fn eval_int_binary_shl_above_bit_width_returns_zero_u32() {
    use crate::constant_fold::eval_int::eval_int_binary;

    assert_eq!(
        eval_int_binary(IntBinaryOp::ShiftLeft, 0xFF, 40, NodeOutputType::U32),
        Some(0),
        "Sleigh: shift > bit-width still returns 0.  Pre-fix fold computed \
         `0xFF << (40 % 32) = 0xFF << 8 = 0xFF00`."
    );
}

/// Sleigh `INT_RIGHT(IntConst(0xFFFF_FFFF), IntConst(32))` at sizeout=4
/// evaluates to 0 — same out-of-range rule as INT_LEFT.
#[test]
fn eval_int_binary_shr_at_bit_width_returns_zero_u32() {
    use crate::constant_fold::eval_int::eval_int_binary;

    assert_eq!(
        eval_int_binary(IntBinaryOp::ShiftRight, 0xFFFF_FFFF, 32, NodeOutputType::U32),
        Some(0),
        "Sleigh: 0xFFFFFFFFu32 >> 32 = 0 per opbehavior.cc:432.  Pre-fix \
         fold computed `0xFFFFFFFF >> (32 % 32) = 0xFFFFFFFF`."
    );
}

/// Sleigh `INT_SRIGHT(IntConst(0xFFFF_FFFF), IntConst(32))` at sizeout=4
/// evaluates to 0xFFFF_FFFF (sign bit set → fill with all-ones).
#[test]
fn eval_int_binary_sshr_at_bit_width_negative_returns_all_ones_u32() {
    use crate::constant_fold::eval_int::eval_int_binary;

    assert_eq!(
        eval_int_binary(IntBinaryOp::SShiftRight, 0xFFFF_FFFF, 32, NodeOutputType::U32),
        Some(0xFFFF_FFFF),
        "Sleigh: signed-negative i32::MAX-style >> 32 fills with sign bit \
         (= 0xFFFFFFFF) per opbehavior.cc:454-460."
    );
}

/// Sleigh `INT_SRIGHT(IntConst(0x7FFF_FFFF), IntConst(32))` at sizeout=4
/// evaluates to 0 (sign bit clear → fill with zeros).
#[test]
fn eval_int_binary_sshr_at_bit_width_positive_returns_zero_u32() {
    use crate::constant_fold::eval_int::eval_int_binary;

    assert_eq!(
        eval_int_binary(IntBinaryOp::SShiftRight, 0x7FFF_FFFF, 32, NodeOutputType::U32),
        Some(0),
        "Sleigh: signed-non-negative >> bit-width = 0 (no sign bit to fill)."
    );
}

