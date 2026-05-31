use super::*;
use crate::opt::pipeline::Optimizer;
use anyhow::anyhow;
use strider_ir::node::{NodeKind, NodeOutputType};
use strider_ir::{
    FloatBinaryOp, FloatCmpOp, FloatUnaryOp,
    IntBinaryOp, IntCmpOp, IntUnaryOp,
};

use crate::opt::test_support::{make_fn, make_fn_with_var, return_kind, return_value};
use strider_ir_test_utils::{reg_vn, RegisterSet};

// ── integer binary folding ────────────────────────────────────────────────

#[test]
fn fold_int_add_consts() -> Result<()> {
    let mut fg = make_fn(|b| {
        let c3 = b.build_int_const(3u64, NodeOutputType::I64).unwrap();
        let c4 = b.build_int_const(4u64, NodeOutputType::I64).unwrap();
        b.build_int_binary_operation(c3, c4, IntBinaryOp::Add, NodeOutputType::I64)
    })?;
    assert!(ConstantFold.optimize(&mut fg)?.changed());
    assert_eq!(return_kind(&fg)?, NodeKind::IntConst(7));
    Ok(())
}

#[test]
fn fold_int_and_zero() -> Result<()> {
    let mut fg = make_fn(|b| {
        let x = b.build_int_const(0xFFu64, NodeOutputType::I64).unwrap();
        let zero = b.build_int_const(0u64, NodeOutputType::I64).unwrap();
        b.build_int_binary_operation(x, zero, IntBinaryOp::And, NodeOutputType::I64)
    })?;
    assert!(ConstantFold.optimize(&mut fg)?.changed());
    assert_eq!(return_kind(&fg)?, NodeKind::IntConst(0));
    Ok(())
}

#[test]
fn fold_int_xor_self() -> Result<()> {
    let mut fg = make_fn(|b| {
        let x = b.build_int_const(0xABu64, NodeOutputType::I64).unwrap();
        b.build_int_binary_operation(x, x, IntBinaryOp::Xor, NodeOutputType::I64)
    })?;
    assert!(ConstantFold.optimize(&mut fg)?.changed());
    assert_eq!(return_kind(&fg)?, NodeKind::IntConst(0));
    Ok(())
}

#[test]
fn fold_int_sub_self() -> Result<()> {
    let mut fg = make_fn(|b| {
        let x = b.build_int_const(0xABu64, NodeOutputType::I64).unwrap();
        b.build_sub_as_add_neg(x, x, NodeOutputType::I64)
    })?;
    assert!(ConstantFold.optimize(&mut fg)?.changed());
    assert_eq!(return_kind(&fg)?, NodeKind::IntConst(0));
    Ok(())
}

#[test]
fn fold_add_zero_identity() -> Result<()> {
    // x + 0 → x  (x is non-const)
    let mut fg = make_fn(|b| {
        let c1 = b.build_int_const(1u64, NodeOutputType::I64).unwrap();
        let c2 = b.build_int_const(2u64, NodeOutputType::I64).unwrap();
        let x = b.build_int_binary_operation(c1, c2, IntBinaryOp::Add, NodeOutputType::I64)?;
        let zero = b.build_int_const(0u64, NodeOutputType::I64).unwrap();
        b.build_int_binary_operation(x, zero, IntBinaryOp::Add, NodeOutputType::I64)
    })?;
    // After at least one fold pass x+0 should collapse to x, then x folds too.
    let mut changed = true;
    while changed {
        changed = ConstantFold.optimize(&mut fg)?.changed();
    }
    assert_eq!(return_kind(&fg)?, NodeKind::IntConst(3));
    Ok(())
}

#[test]
fn fold_mul_by_one() -> Result<()> {
    let mut fg = make_fn(|b| {
        let c5 = b.build_int_const(5u64, NodeOutputType::I64).unwrap();
        let one = b.build_int_const(1u64, NodeOutputType::I64).unwrap();
        b.build_int_binary_operation(c5, one, IntBinaryOp::Mul, NodeOutputType::I64)
    })?;
    assert!(ConstantFold.optimize(&mut fg)?.changed());
    assert_eq!(return_kind(&fg)?, NodeKind::IntConst(5));
    Ok(())
}

/// `(x & 4) & 7`  — bit 2 is the only bit reachable by both masks, so the
/// merged constant is `4 & 7 = 4`.
#[test]
fn fold_and_and_masks() -> Result<()> {
    let mut fg = make_fn(|b| {
        let x = b.build_int_const(0xFFu64, NodeOutputType::I64).unwrap();
        let c4 = b.build_int_const(4u64, NodeOutputType::I64).unwrap();
        let c7 = b.build_int_const(7u64, NodeOutputType::I64).unwrap();
        let inner =
            b.build_int_binary_operation(x, c4, IntBinaryOp::And, NodeOutputType::I64)?;
        b.build_int_binary_operation(inner, c7, IntBinaryOp::And, NodeOutputType::I64)
    })?;
    // Run to convergence (both-const fold + mask-merge may each fire once).
    let mut changed = true;
    while changed {
        changed = ConstantFold.optimize(&mut fg)?.changed();
    }
    // 0xFF & 4 = 4, 4 & 7 = 4.
    assert_eq!(return_kind(&fg)?, NodeKind::IntConst(4));
    Ok(())
}

// ── add/sub reassociation with constants ──────────────────────────────────

/// Asserts the return-value node is `expected_base + expected_const`
/// (type-masked; operand order irrelevant).
fn assert_add_with_const(
    fg: &strider_ir::Graph,
    expected_base: strider_ir::Value,
    expected_const: u64,
    ty: NodeOutputType,
) -> Result<()> {
    let val = return_value(fg)?;
    let node = fg.node_for_output(val);
    assert!(
        matches!(
            fg.node_kind(node),
            NodeKind::IntBinaryOp(IntBinaryOp::Add)
        ),
        "expected outer Add, got {:?}",
        fg.node_kind(node)
    );
    let inputs = fg.node_inputs(node);
    assert_eq!(inputs.len(), 2);
    let l = inputs[0];
    let r = inputs[1];
    let masked = ty
        .get_unsigned_int(u128::from(expected_const))
        .ok_or_else(|| anyhow!("expected integer type, got {ty:?}"))?;
    let const_on = |o: strider_ir::Value| -> bool {
        matches!(
            *fg.kind_of_output(o),
            // IntConst stores u128; masked is u64, widen for comparison.
            NodeKind::IntConst(v) if ty.get_unsigned_int(v) == Some(masked)
        )
    };
    let ok = (l == expected_base && const_on(r)) || (r == expected_base && const_on(l));
    assert!(
        ok,
        "expected `base + {:#x}`; got lhs kind={:?}, rhs kind={:?}",
        masked,
        fg.kind_of_output(l),
        fg.kind_of_output(r),
    );
    Ok(())
}

/// Asserts the return-value node is `expected_base - expected_const` in the
/// canonical post-fold lowered shape: `Add(expected_base, IntConst(-K))`,
/// where `-K` is `wrapping_neg(expected_const)` masked to the type's width.
///
/// `IntBinaryOp::Sub` is not a primitive in this IR — pcode-lift produces
/// `Add(a, Neg(b))` and `ConstantFold` collapses `Neg(IntConst(K))` to
/// `IntConst(-K)`, leaving a single `Add` node with a negative-valued constant.
fn assert_sub_with_const(
    fg: &strider_ir::Graph,
    expected_base: strider_ir::Value,
    expected_const: u64,
    ty: NodeOutputType,
) -> Result<()> {
    let val = return_value(fg)?;
    let node = fg.node_for_output(val);
    assert!(
        matches!(
            fg.node_kind(node),
            NodeKind::IntBinaryOp(IntBinaryOp::Add)
        ),
        "expected outer Add (lowered Sub), got {:?}",
        fg.node_kind(node)
    );
    let inputs = fg.node_inputs(node);
    assert_eq!(inputs.len(), 2);
    // The lowered `a - K` shape after `ConstantFold` is `Add(a, IntConst(-K))`.
    // `Add` is commutative, so accept the const on either side; check that
    // the captured constant equals `wrapping_neg(K)` masked to the type's width.
    let l = inputs[0];
    let r = inputs[1];
    let neg_masked = ty
        .get_unsigned_int(u128::from(expected_const).wrapping_neg())
        .ok_or_else(|| anyhow!("expected integer type, got {ty:?}"))?;
    let const_match = |out: strider_ir::Value| {
        matches!(
            *fg.kind_of_output(out),
            NodeKind::IntConst(v) if ty.get_unsigned_int(v) == Some(neg_masked)
        )
    };
    let ok = (l == expected_base && const_match(r))
        || (r == expected_base && const_match(l));
    assert!(
        ok,
        "expected `base + {:#x}` (= base - {:#x} canonicalised); got lhs kind={:?}, rhs kind={:?}",
        neg_masked,
        expected_const,
        fg.kind_of_output(l),
        fg.kind_of_output(r),
    );
    Ok(())
}

#[test]
fn reassoc_add_add_consts() -> Result<()> {
    // (x + 3) + 4 → x + 7
    let vn = reg_vn(0x1000, 8);
    let (mut fg, x) = make_fn_with_var(vn, |b, x| {
        let c3 = b.build_int_const(3u64, NodeOutputType::I64).unwrap();
        let c4 = b.build_int_const(4u64, NodeOutputType::I64).unwrap();
        let inner =
            b.build_int_binary_operation(x, c3, IntBinaryOp::Add, NodeOutputType::I64)?;
        b.build_int_binary_operation(inner, c4, IntBinaryOp::Add, NodeOutputType::I64)
    })?;
    let mut changed = true;
    while changed {
        changed = ConstantFold.optimize(&mut fg)?.changed();
    }
    assert_add_with_const(&fg, x, 7, NodeOutputType::I64)?;
    Ok(())
}

#[test]
fn reassoc_add_sub_consts() -> Result<()> {
    // (x - 3) + 4 → x + 1
    let vn = reg_vn(0x1000, 8);
    let (mut fg, x) = make_fn_with_var(vn, |b, x| {
        let c3 = b.build_int_const(3u64, NodeOutputType::I64).unwrap();
        let c4 = b.build_int_const(4u64, NodeOutputType::I64).unwrap();
        let inner =
            b.build_sub_as_add_neg(x, c3, NodeOutputType::I64)?;
        b.build_int_binary_operation(inner, c4, IntBinaryOp::Add, NodeOutputType::I64)
    })?;
    let mut changed = true;
    while changed {
        changed = ConstantFold.optimize(&mut fg)?.changed();
    }
    assert_add_with_const(&fg, x, 1, NodeOutputType::I64)?;
    Ok(())
}

#[test]
fn reassoc_sub_add_consts_wrapping() -> Result<()> {
    // (x + 3) - 4 → x + (3 - 4)  = x + 0xFFFF_FFFF_FFFF_FFFF
    let vn = reg_vn(0x1000, 8);
    let (mut fg, x) = make_fn_with_var(vn, |b, x| {
        let c3 = b.build_int_const(3u64, NodeOutputType::I64).unwrap();
        let c4 = b.build_int_const(4u64, NodeOutputType::I64).unwrap();
        let inner =
            b.build_int_binary_operation(x, c3, IntBinaryOp::Add, NodeOutputType::I64)?;
        b.build_sub_as_add_neg(inner, c4, NodeOutputType::I64)
    })?;
    let mut changed = true;
    while changed {
        changed = ConstantFold.optimize(&mut fg)?.changed();
    }
    assert_add_with_const(&fg, x, 0xFFFF_FFFF_FFFF_FFFF, NodeOutputType::I64)?;
    Ok(())
}

#[test]
fn reassoc_sub_sub_consts() -> Result<()> {
    // (x - 3) - 4 → x - 7
    let vn = reg_vn(0x1000, 8);
    let (mut fg, x) = make_fn_with_var(vn, |b, x| {
        let c3 = b.build_int_const(3u64, NodeOutputType::I64).unwrap();
        let c4 = b.build_int_const(4u64, NodeOutputType::I64).unwrap();
        let inner =
            b.build_sub_as_add_neg(x, c3, NodeOutputType::I64)?;
        b.build_sub_as_add_neg(inner, c4, NodeOutputType::I64)
    })?;
    let mut changed = true;
    while changed {
        changed = ConstantFold.optimize(&mut fg)?.changed();
    }
    assert_sub_with_const(&fg, x, 7, NodeOutputType::I64)?;
    Ok(())
}

#[test]
fn reassoc_add_commuted_inner() -> Result<()> {
    // (3 + x) + 4 → x + 7 (inner Add has const on lhs)
    let vn = reg_vn(0x1000, 8);
    let (mut fg, x) = make_fn_with_var(vn, |b, x| {
        let c3 = b.build_int_const(3u64, NodeOutputType::I64).unwrap();
        let c4 = b.build_int_const(4u64, NodeOutputType::I64).unwrap();
        let inner =
            b.build_int_binary_operation(c3, x, IntBinaryOp::Add, NodeOutputType::I64)?;
        b.build_int_binary_operation(inner, c4, IntBinaryOp::Add, NodeOutputType::I64)
    })?;
    let mut changed = true;
    while changed {
        changed = ConstantFold.optimize(&mut fg)?.changed();
    }
    assert_add_with_const(&fg, x, 7, NodeOutputType::I64)?;
    Ok(())
}

#[test]
fn reassoc_add_commuted_outer() -> Result<()> {
    // 4 + (x + 3) → x + 7 (outer Add has const on lhs)
    let vn = reg_vn(0x1000, 8);
    let (mut fg, x) = make_fn_with_var(vn, |b, x| {
        let c3 = b.build_int_const(3u64, NodeOutputType::I64).unwrap();
        let c4 = b.build_int_const(4u64, NodeOutputType::I64).unwrap();
        let inner =
            b.build_int_binary_operation(x, c3, IntBinaryOp::Add, NodeOutputType::I64)?;
        b.build_int_binary_operation(c4, inner, IntBinaryOp::Add, NodeOutputType::I64)
    })?;
    let mut changed = true;
    while changed {
        changed = ConstantFold.optimize(&mut fg)?.changed();
    }
    assert_add_with_const(&fg, x, 7, NodeOutputType::I64)?;
    Ok(())
}

#[test]
fn reassoc_chain_three_subs() -> Result<()> {
    // ((x - 4) - 4) - 4 → x - 12.  Requires the fixed-point loop to
    // compose multiple reassociation steps.
    let vn = reg_vn(0x1000, 8);
    let (mut fg, x) = make_fn_with_var(vn, |b, x| {
        let c4 = b.build_int_const(4u64, NodeOutputType::I64).unwrap();
        let a = b.build_sub_as_add_neg(x, c4, NodeOutputType::I64)?;
        let b_ = b.build_sub_as_add_neg(a, c4, NodeOutputType::I64)?;
        b.build_sub_as_add_neg(b_, c4, NodeOutputType::I64)
    })?;
    let mut changed = true;
    while changed {
        changed = ConstantFold.optimize(&mut fg)?.changed();
    }
    assert_sub_with_const(&fg, x, 12, NodeOutputType::I64)?;
    Ok(())
}

#[test]
fn reassoc_chain_three_subs_u32() -> Result<()> {
    // Same chain but at I32: ((x - 4) - 4) - 4 → x - 12.
    let vn = reg_vn(0x1000, 4);
    let (mut fg, x) = make_fn_with_var(vn, |b, x| {
        let c4 = b.build_int_const(4u64, NodeOutputType::I32).unwrap();
        let a = b.build_sub_as_add_neg(x, c4, NodeOutputType::I32)?;
        let b_ = b.build_sub_as_add_neg(a, c4, NodeOutputType::I32)?;
        b.build_sub_as_add_neg(b_, c4, NodeOutputType::I32)
    })?;
    let mut changed = true;
    while changed {
        changed = ConstantFold.optimize(&mut fg)?.changed();
    }
    assert_sub_with_const(&fg, x, 12, NodeOutputType::I32)?;
    Ok(())
}

#[test]
fn reassoc_no_fold_without_const() -> Result<()> {
    // (x + y) + z, no constants → untouched.
    let xv = reg_vn(0x1000, 8);
    let yv = reg_vn(0x1008, 8);
    let zv = reg_vn(0x1010, 8);
    let mut b = RegisterSet::new()
        .tracked(xv)
        .tracked(yv)
        .tracked(zv)
        .arg(xv)
        .arg(yv)
        .arg(zv)
        .build_fn_single_region()?;
    let x = b.read_variable(&xv)?;
    let y = b.read_variable(&yv)?;
    let z = b.read_variable(&zv)?;
    let inner = b.build_int_binary_operation(x, y, IntBinaryOp::Add, NodeOutputType::I64)?;
    let outer =
        b.build_int_binary_operation(inner, z, IntBinaryOp::Add, NodeOutputType::I64)?;
    b.build_return(Some(outer), &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;
    let before = return_value(&fg)?;
    // Should not change: no constants anywhere.
    let res = ConstantFold.optimize(&mut fg)?;
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
    let mut b = RegisterSet::new()
        .tracked(av)
        .tracked(bv)
        .arg(av)
        .arg(bv)
        .build_fn_single_region()?;
    let a = b.read_variable(&av)?;
    let bval = b.read_variable(&bv)?;
    let f0 = b.build_int_const(0xF0u64, NodeOutputType::I64).unwrap();
    let f0_ = b.build_int_const(0x0Fu64, NodeOutputType::I64).unwrap();
    let ff = b.build_int_const(0xFFu64, NodeOutputType::I64).unwrap();
    let a_and_f0 =
        b.build_int_binary_operation(a, f0, IntBinaryOp::And, NodeOutputType::I64)?;
    let b_and_0f =
        b.build_int_binary_operation(bval, f0_, IntBinaryOp::And, NodeOutputType::I64)?;
    let or_node =
        b.build_int_binary_operation(a_and_f0, b_and_0f, IntBinaryOp::Or, NodeOutputType::I64)?;
    let outer =
        b.build_int_binary_operation(or_node, ff, IntBinaryOp::And, NodeOutputType::I64)?;
    b.build_return(Some(outer), &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;
    let changed = ConstantFold.optimize(&mut fg)?.changed();
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
        let wide = b.build_int_const(0xFF00u64, NodeOutputType::I16).unwrap();
        b.truncate_if_needed(wide, NodeOutputType::I8)
    })?;
    let val = return_value(&fg)?;
    // Use int_const_val which masks to the declared type.
    let semantic = fg.int_const_val(val);
    assert_eq!(semantic, Some(0), "0xFF00 truncated to I8 should be 0");
    // No Truncate nodes should exist.
    assert!(
        !fg.all_node_ids()
            .any(|n| matches!(fg.node_kind(n), NodeKind::Truncate)),
        "builder should have folded the truncate"
    );
    Ok(())
}

/// Rule 4 (`Truncate(IntConst(v)) => int_const(v, ty)`) must mask the
/// stored value to the truncate's output width. Otherwise the IR-layer
/// invariant "an IntConst's stored value fits its declared type" silently
/// breaks: a `Truncate(IntConst(0xFFFF, I16))` would rewrite to
/// `IntConst(0xFFFF, I8)` (typed-narrow but value-wide).
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
        let a = b.build_int_const(0xFFFFu64, NodeOutputType::I16).unwrap();
        let b_ = b.build_int_const(0xFFFFu64, NodeOutputType::I16).unwrap();
        // Non-const node so truncate_if_needed emits a real Truncate node.
        let or = b.build_int_binary_operation(a, b_, IntBinaryOp::Or, NodeOutputType::I16)?;
        b.truncate_if_needed(or, NodeOutputType::I8)
    })?;
    // Sanity: builder did emit a Truncate node.
    assert!(
        fg.all_node_ids()
            .any(|n| matches!(fg.node_kind(n), NodeKind::Truncate)),
        "test setup expects a Truncate node before optimization",
    );

    assert!(ConstantFold.optimize(&mut fg)?.changed());

    // After optimization the Return's value must be an `IntConst(0xFF)`,
    // i.e. the low byte of 0xFFFF — *masked* to I8. A pre-fix run would
    // store `0xFFFF` (the wider raw value) here.
    let val = return_value(&fg)?;
    let kind = *fg.kind_of_output(val);
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

// ── Truncate(Extend(x)) round-trip ───────────────────────────────────────
//
// Register-merge chains in `write_reg_vn` produce
//   Extend_zext(Truncate(Or(...)))
// and similar `Truncate(Extend(x))` round-trips when the inner expression's
// width equals the outer truncate's output width.  The round-trip rules in
// `apply_bitcast_extend_rules` collapse these to the inner expression.
// These do NOT cover the opposite direction (Extend/Truncate at the *outer*
// level, which still defeats the matcher's data-flow walk on x86 IMUL
// chains), but they ARE valid algebraic identities that simplify the IR
// generally.

use strider_ir::ExtendOp;

/// `Truncate_<W>(ZeroExtend_<W'>(x_<W>))` where `x` already has type `W`
/// must collapse to `x` — the extend added zero bits that the truncate
/// cuts off, so the round-trip is identity.
#[test]
fn fold_truncate_of_zero_extend_round_trip() -> Result<()> {
    let mut fg = make_fn(|b| {
        // Non-const I32 expression so the builder can't short-circuit.
        let a = b.build_int_const(0xAAu64, NodeOutputType::I32).unwrap();
        let bb = b.build_int_const(0x55u64, NodeOutputType::I32).unwrap();
        let or = b.build_int_binary_operation(a, bb, IntBinaryOp::Or, NodeOutputType::I32)?;
        let widened = b.extend_if_needed(or, NodeOutputType::I64, ExtendOp::ZeroExtend)?;
        b.truncate_if_needed(widened, NodeOutputType::I32)
    })?;
    let mut changed = true;
    while changed {
        changed = ConstantFold.optimize(&mut fg)?.changed();
    }
    // After optimization the Or's two const inputs fold to IntConst(0xFF),
    // and the Truncate(Extend(IntConst(0xFF))) collapses to IntConst(0xFF).
    // Most importantly: no Truncate or Extend node remains in the chain.
    let val = return_value(&fg)?;
    assert!(
        matches!(fg.kind_of_output(val), NodeKind::IntConst(_)),
        "round-trip + const-fold must leave an IntConst at the root, got {:?}",
        fg.kind_of_output(val)
    );
    // Belt-and-suspenders: walk all reachable nodes and verify no
    // Truncate/Extend survives the chain to the Return.
    for nid in fg.walk() {
        let kind = fg.node_kind(nid);
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
        let a = b.build_int_const(0x80u64, NodeOutputType::I32).unwrap();
        let bb = b.build_int_const(0x01u64, NodeOutputType::I32).unwrap();
        let or = b.build_int_binary_operation(a, bb, IntBinaryOp::Or, NodeOutputType::I32)?;
        let widened = b.extend_if_needed(or, NodeOutputType::I64, ExtendOp::SignExtend)?;
        b.truncate_if_needed(widened, NodeOutputType::I32)
    })?;
    let mut changed = true;
    while changed {
        changed = ConstantFold.optimize(&mut fg)?.changed();
    }
    for nid in fg.walk() {
        let kind = fg.node_kind(nid);
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
        let lhs = b.build_int_const(3u64, NodeOutputType::I32).unwrap();
        let rhs = b.build_int_const(7u64, NodeOutputType::I32).unwrap();
        // Use non-const expressions so the constant folder doesn't
        // collapse before our rule runs.
        let lhs_or = b.build_int_binary_operation(lhs, lhs, IntBinaryOp::Or, NodeOutputType::I32)?;
        let rhs_or = b.build_int_binary_operation(rhs, rhs, IntBinaryOp::Or, NodeOutputType::I32)?;
        let lhs_ext = b.extend_if_needed(lhs_or, NodeOutputType::I64, ExtendOp::SignExtend)?;
        let rhs_ext = b.extend_if_needed(rhs_or, NodeOutputType::I64, ExtendOp::SignExtend)?;
        let mul = b.build_int_binary_operation(lhs_ext, rhs_ext, IntBinaryOp::Mul, NodeOutputType::I64)?;
        b.truncate_if_needed(mul, NodeOutputType::I32)
    })?;
    let mut changed = true;
    while changed {
        changed = ConstantFold.optimize(&mut fg)?.changed();
    }
    // After narrowing-through-Mul + constant fold: 3 * 7 = 21 at I32.
    assert_eq!(return_kind(&fg)?, NodeKind::IntConst(21));
    // Nothing wider than I32 should survive (no SignExtend/Mul@I64/Truncate).
    for nid in fg.walk() {
        let kind = fg.node_kind(nid);
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
        let low_part = b.build_int_const(0xAAu64, NodeOutputType::I64).unwrap();
        let junk = b.build_int_const(0x12345678_DEADBEEFu64, NodeOutputType::I64).unwrap();
        // Make low_part non-const via Or so the rule fires through it.
        let low_or = b.build_int_binary_operation(
            low_part, low_part, IntBinaryOp::Or, NodeOutputType::I64)?;
        // High mask = 0xFFFF_FFFF_0000_0000 (low 32 bits are zero).
        let high_mask = b.build_int_const(0xFFFFFFFF_00000000u64, NodeOutputType::I64).unwrap();
        let high_part = b.build_int_binary_operation(
            high_mask, junk, IntBinaryOp::And, NodeOutputType::I64)?;
        let merged = b.build_int_binary_operation(
            low_or, high_part, IntBinaryOp::Or, NodeOutputType::I64)?;
        b.truncate_if_needed(merged, NodeOutputType::I32)
    })?;
    let mut changed = true;
    while changed {
        changed = ConstantFold.optimize(&mut fg)?.changed();
    }
    // After dropping the high half + folding 0xAA | 0xAA = 0xAA at I32:
    // the result is IntConst(0xAA).  No Or remains.
    assert_eq!(return_kind(&fg)?, NodeKind::IntConst(0xAA));
    for nid in fg.walk() {
        let kind = fg.node_kind(nid);
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
        // x is a non-const I64 expression.
        let a = b.build_int_const(0x1234_5678_DEAD_BEEFu64, NodeOutputType::I64).unwrap();
        let x = b.build_int_binary_operation(a, a, IntBinaryOp::Or, NodeOutputType::I64)?;
        let low_mask = b.build_int_const(0xFFFFFFFFu64, NodeOutputType::I64).unwrap();
        let masked = b.build_int_binary_operation(
            low_mask, x, IntBinaryOp::And, NodeOutputType::I64)?;
        b.truncate_if_needed(masked, NodeOutputType::I32)
    })?;
    let mut changed = true;
    while changed {
        changed = ConstantFold.optimize(&mut fg)?.changed();
    }
    // After dropping the redundant And + folding the OR-of-itself:
    // result is IntConst(0xDEADBEEF) at I32.
    assert_eq!(return_kind(&fg)?, NodeKind::IntConst(0xDEADBEEF));
    Ok(())
}

/// The round-trip rule must NOT fire when `x`'s type is *narrower* than
/// the truncate's output type — that's a real width-narrowing operation,
/// not an identity.  `Truncate_U16(Extend_U64(x_U32))` is still a real
/// truncation from I32 to I16.
#[test]
fn fold_truncate_of_extend_skips_when_widths_differ() -> Result<()> {
    let mut fg = make_fn(|b| {
        let a = b.build_int_const(0xAAu64, NodeOutputType::I32).unwrap();
        let bb = b.build_int_const(0x55u64, NodeOutputType::I32).unwrap();
        let or = b.build_int_binary_operation(a, bb, IntBinaryOp::Or, NodeOutputType::I32)?;
        let widened = b.extend_if_needed(or, NodeOutputType::I64, ExtendOp::ZeroExtend)?;
        // Truncate to I16 — narrower than the inner Or's I32 width, so the
        // round-trip rule must NOT fire.  Constant-fold can still collapse
        // the const-Or, but the truncate must remain (or its result must
        // still semantically be I16).
        b.truncate_if_needed(widened, NodeOutputType::I16)
    })?;
    let mut changed = true;
    while changed {
        changed = ConstantFold.optimize(&mut fg)?.changed();
    }
    // The result must be I16-typed.
    let val = return_value(&fg)?;
    assert_eq!(
        fg.output_kind(val),
        strider_ir::node::NodeOutputKind::OutputType(NodeOutputType::I16),
        "Truncate_U16(Extend_U64(I32)) must keep I16 typing — round-trip \
         rule must not fire when inner width != outer truncate width"
    );
    Ok(())
}

// ── boolean folding ───────────────────────────────────────────────────────

// Booleans are 1-bit (`I1`) integers: a boolean NOT is now
// `Xor(x, IntConst(1)):I1` (since the former BitNot unary-op was removed in
// favour of `Xor(_, all_ones)`), AND/OR/XOR are `IntBinaryOp` at I1, and a
// boolean const is an `IntConst(0|1)` typed I1.  These folds now flow
// through the generic integer const-fold / identity rules at I1.

#[test]
fn fold_bool_neg_const() -> Result<()> {
    // `!true` = `Xor(IntConst(1):I1, IntConst(1):I1)` folds to
    // `IntConst(0):I1` via the integer binary const-fold rule.
    let mut fg = make_fn(|b| {
        let t = b.build_boolean_const(true);
        let one = b.build_all_ones_const(NodeOutputType::I1)?;
        b.build_int_binary_operation(t, one, IntBinaryOp::Xor, NodeOutputType::I1)
    })?;
    assert!(ConstantFold.optimize(&mut fg)?.changed());
    assert_eq!(return_kind(&fg)?, NodeKind::IntConst(0));
    Ok(())
}

#[test]
fn fold_bool_and_consts() -> Result<()> {
    // `true & false` folds to `IntConst(0):I1` via the integer binary
    // const-fold rule.
    let mut fg = make_fn(|b| {
        let t = b.build_boolean_const(true);
        let f = b.build_boolean_const(false);
        b.build_int_binary_operation(t, f, IntBinaryOp::And, NodeOutputType::I1)
    })?;
    assert!(ConstantFold.optimize(&mut fg)?.changed());
    assert_eq!(return_kind(&fg)?, NodeKind::IntConst(0));
    Ok(())
}

// `xor b true` is the canonical logical-NOT shape post-the former BitNot unary-op
// removal — building it directly already yields `~b`, so no rewrite is
// needed.  Pin that the const-fold pipeline leaves the Xor in place (no
// `x ^ all_ones → ~x` rewrite anymore — that canonicalisation collapsed
// into the lift-time shape) when `b` is non-const.
#[test]
fn fold_bool_xor_true_to_not() -> Result<()> {
    let vn = reg_vn(0x1000, 8);
    let (mut fg, _x) = make_fn_with_var(vn, |b, x| {
        let c5 = b.build_int_const(5u64, NodeOutputType::I64).unwrap();
        // Non-const Bool: `x == 5`.
        let cmp = b.build_int_cmp_operation(x, c5, IntCmpOp::Equal, NodeOutputType::I64)?;
        let t = b.build_boolean_const(true);
        b.build_int_binary_operation(cmp, t, IntBinaryOp::Xor, NodeOutputType::I1)
    })?;
    // The const-fold pipeline may or may not "change" — the Xor shape
    // is already canonical for logical-NOT.  Assert the final shape.
    let _ = ConstantFold.optimize(&mut fg)?;
    assert_eq!(return_kind(&fg)?, NodeKind::IntBinaryOp(IntBinaryOp::Xor));
    Ok(())
}

// `xor` is commutative, so `true ^ b` must rewrite the same as `b ^ true`.
#[test]
fn fold_bool_true_xor_x_to_not_commutative() -> Result<()> {
    let vn = reg_vn(0x1000, 8);
    let (mut fg, _x) = make_fn_with_var(vn, |b, x| {
        let c5 = b.build_int_const(5u64, NodeOutputType::I64).unwrap();
        let cmp = b.build_int_cmp_operation(x, c5, IntCmpOp::Equal, NodeOutputType::I64)?;
        let t = b.build_boolean_const(true);
        // Operands flipped relative to the previous test.
        b.build_int_binary_operation(t, cmp, IntBinaryOp::Xor, NodeOutputType::I1)
    })?;
    let _ = ConstantFold.optimize(&mut fg)?;
    assert_eq!(return_kind(&fg)?, NodeKind::IntBinaryOp(IntBinaryOp::Xor));
    Ok(())
}

// `xor b false` does not match the `x ^ all_ones → ~x` canonicalization
// (the const is `0`, not all-ones).  Instead the integer identity rule
// `x ^ 0 → x` fires, collapsing it to the cmp directly.
#[test]
fn no_fold_bool_xor_false() -> Result<()> {
    let vn = reg_vn(0x1000, 8);
    let (mut fg, _x) = make_fn_with_var(vn, |b, x| {
        let c5 = b.build_int_const(5u64, NodeOutputType::I64).unwrap();
        let cmp = b.build_int_cmp_operation(x, c5, IntCmpOp::Equal, NodeOutputType::I64)?;
        let f = b.build_boolean_const(false);
        b.build_int_binary_operation(cmp, f, IntBinaryOp::Xor, NodeOutputType::I1)
    })?;
    assert!(ConstantFold.optimize(&mut fg)?.changed());
    // `x ^ 0 → x`: the Xor collapses to the cmp, not to a BitNot.
    assert_eq!(return_kind(&fg)?, NodeKind::IntCmpOp(IntCmpOp::Equal));
    Ok(())
}

// `x | true → true` for non-const x (Or-absorbing at I1; `true` is the
// all-ones value).  Pins the re-expressed `BOr(true, _) → true` rule —
// `Or` is commutative so `true | x` rewrites the same.
#[test]
fn fold_bool_or_true_to_true() -> Result<()> {
    let vn = reg_vn(0x1000, 8);
    let (mut fg, _x) = make_fn_with_var(vn, |b, x| {
        let c5 = b.build_int_const(5u64, NodeOutputType::I64).unwrap();
        // Non-const Bool: `x == 5`.
        let cmp = b.build_int_cmp_operation(x, c5, IntCmpOp::Equal, NodeOutputType::I64)?;
        let t = b.build_boolean_const(true);
        b.build_int_binary_operation(cmp, t, IntBinaryOp::Or, NodeOutputType::I1)
    })?;
    assert!(ConstantFold.optimize(&mut fg)?.changed());
    // `x | true → true`: folds to the constant 1 (true at I1), not the cmp.
    assert_eq!(return_kind(&fg)?, NodeKind::IntConst(1));
    Ok(())
}

// `x | all_ones → all_ones` at a wide width (I32): the general integer
// Or-absorbing rule folds to the all-ones constant (it is NOT limited to
// the I1 boolean `true`).
#[test]
fn fold_int_or_all_ones_to_all_ones() -> Result<()> {
    let vn = reg_vn(0x1000, 4); // 4-byte var → I32
    let (mut fg, _x) = make_fn_with_var(vn, |b, x| {
        // Non-const I32 value via `Neg` (two's-complement negate), then `| 0xFFFF_FFFF`.
        let x32 = b.build_int_unary_operation(x, IntUnaryOp::Neg, NodeOutputType::I32)?;
        let all_ones = b.build_int_const(0xFFFF_FFFFu64, NodeOutputType::I32).unwrap();
        b.build_int_binary_operation(x32, all_ones, IntBinaryOp::Or, NodeOutputType::I32)
    })?;
    assert!(ConstantFold.optimize(&mut fg)?.changed());
    // Folds to the all-ones constant.
    assert_eq!(return_kind(&fg)?, NodeKind::IntConst(0xFFFF_FFFF));
    Ok(())
}

// `!!x → x` for non-const x — at I1 each logical NOT is
// `Xor(_, IntConst(1)):I1`, so this builds `Xor(Xor(cmp, 1), 1)` which
// the `bool_not(bool_not(x)) → x` rule (or the xor reassoc rules)
// collapses back to the cmp.
#[test]
fn fold_bool_double_not_to_x() -> Result<()> {
    let vn = reg_vn(0x1000, 8);
    let (mut fg, _x) = make_fn_with_var(vn, |b, x| {
        let c5 = b.build_int_const(5u64, NodeOutputType::I64).unwrap();
        let cmp = b.build_int_cmp_operation(x, c5, IntCmpOp::Equal, NodeOutputType::I64)?;
        let one = b.build_all_ones_const(NodeOutputType::I1)?;
        let n1 = b.build_int_binary_operation(cmp, one, IntBinaryOp::Xor, NodeOutputType::I1)?;
        b.build_int_binary_operation(n1, one, IntBinaryOp::Xor, NodeOutputType::I1)
    })?;
    assert!(ConstantFold.optimize(&mut fg)?.changed());
    // After fold the function returns the cmp directly.
    assert_eq!(return_kind(&fg)?, NodeKind::IntCmpOp(IntCmpOp::Equal));
    Ok(())
}

// Composes with the xor-true rule via the fixed-point loop:
// `xor (xor b true) true` → `not (not b)` → `b`.
#[test]
fn fold_bool_xor_true_xor_true_collapses_to_x() -> Result<()> {
    let vn = reg_vn(0x1000, 8);
    let (mut fg, _x) = make_fn_with_var(vn, |b, x| {
        let c5 = b.build_int_const(5u64, NodeOutputType::I64).unwrap();
        let cmp = b.build_int_cmp_operation(x, c5, IntCmpOp::Equal, NodeOutputType::I64)?;
        let t1 = b.build_boolean_const(true);
        let xor1 = b.build_int_binary_operation(cmp, t1, IntBinaryOp::Xor, NodeOutputType::I1)?;
        let t2 = b.build_boolean_const(true);
        b.build_int_binary_operation(xor1, t2, IntBinaryOp::Xor, NodeOutputType::I1)
    })?;
    let mut changed = true;
    while changed {
        changed = ConstantFold.optimize(&mut fg)?.changed();
    }
    assert_eq!(return_kind(&fg)?, NodeKind::IntCmpOp(IntCmpOp::Equal));
    Ok(())
}

// ── no-fold edge cases ────────────────────────────────────────────────────

#[test]
fn no_fold_div_by_zero() -> Result<()> {
    let mut fg = make_fn(|b| {
        let x = b.build_int_const(10u64, NodeOutputType::I64).unwrap();
        let zero = b.build_int_const(0u64, NodeOutputType::I64).unwrap();
        b.build_int_binary_operation(x, zero, IntBinaryOp::Div, NodeOutputType::I64)
    })?;
    // Should not fold (division by zero is undefined).
    assert!(!ConstantFold.optimize(&mut fg)?.changed());
    assert!(matches!(
        return_kind(&fg)?,
        NodeKind::IntBinaryOp(IntBinaryOp::Div)
    ));
    Ok(())
}

#[test]
fn fold_int_cmp_equal_consts() -> Result<()> {
    let mut fg = make_fn(|b| {
        let c5 = b.build_int_const(5u64, NodeOutputType::I64).unwrap();
        let c5b = b.build_int_const(5u64, NodeOutputType::I64).unwrap();
        b.build_int_cmp_operation(c5, c5b, IntCmpOp::Equal, NodeOutputType::I64)
    })?;
    assert!(ConstantFold.optimize(&mut fg)?.changed());
    assert_eq!(return_kind(&fg)?, NodeKind::IntConst(1));
    Ok(())
}

#[test]
fn fold_int_cmp_less_consts() -> Result<()> {
    let mut fg = make_fn(|b| {
        let c3 = b.build_int_const(3u64, NodeOutputType::I64).unwrap();
        let c5 = b.build_int_const(5u64, NodeOutputType::I64).unwrap();
        b.build_int_cmp_operation(c3, c5, IntCmpOp::Less, NodeOutputType::I64)
    })?;
    assert!(ConstantFold.optimize(&mut fg)?.changed());
    assert_eq!(return_kind(&fg)?, NodeKind::IntConst(1));
    Ok(())
}

// ── Popcount / Lzcount ────────────────────────────────────────────────────

#[test]
fn fold_popcount_const() -> Result<()> {
    // popcount(0b10110101) = 5
    let mut fg = make_fn(|b| {
        let v = b.build_int_const(0b10110101u64, NodeOutputType::I8).unwrap();
        b.build_popcount(v, NodeOutputType::I8)
    })?;
    assert!(ConstantFold.optimize(&mut fg)?.changed());
    assert_eq!(return_kind(&fg)?, NodeKind::IntConst(5));
    Ok(())
}

#[test]
fn fold_popcount_zero() -> Result<()> {
    let mut fg = make_fn(|b| {
        let v = b.build_int_const(0u64, NodeOutputType::I64).unwrap();
        b.build_popcount(v, NodeOutputType::I64)
    })?;
    assert!(ConstantFold.optimize(&mut fg)?.changed());
    assert_eq!(return_kind(&fg)?, NodeKind::IntConst(0));
    Ok(())
}

#[test]
fn fold_lzcount_msb_set() -> Result<()> {
    // lzcount(0x80u8) = 0 (MSB is set)
    let mut fg = make_fn(|b| {
        let v = b.build_int_const(0x80u64, NodeOutputType::I8).unwrap();
        b.build_lzcount(v, NodeOutputType::I8)
    })?;
    assert!(ConstantFold.optimize(&mut fg)?.changed());
    assert_eq!(return_kind(&fg)?, NodeKind::IntConst(0));
    Ok(())
}

#[test]
fn fold_lzcount_one() -> Result<()> {
    // lzcount(1u8) = 7 (only bit 0 set in an 8-bit value)
    let mut fg = make_fn(|b| {
        let v = b.build_int_const(1u64, NodeOutputType::I8).unwrap();
        b.build_lzcount(v, NodeOutputType::I8)
    })?;
    assert!(ConstantFold.optimize(&mut fg)?.changed());
    assert_eq!(return_kind(&fg)?, NodeKind::IntConst(7));
    Ok(())
}

#[test]
fn fold_lzcount_zero_u32() -> Result<()> {
    // lzcount(0_U32) must fold to 32 (the type's bit width). The previous
    // formula `(masked << (64 - bits)).leading_zeros()` returned 64 when
    // masked was 0, ignoring the type's narrower width.
    let mut fg = make_fn(|b| {
        let v = b.build_int_const(0u64, NodeOutputType::I32).unwrap();
        b.build_lzcount(v, NodeOutputType::I32)
    })?;
    assert!(ConstantFold.optimize(&mut fg)?.changed());
    assert_eq!(return_kind(&fg)?, NodeKind::IntConst(32));
    Ok(())
}

#[test]
fn fold_lzcount_zero_u8() -> Result<()> {
    let mut fg = make_fn(|b| {
        let v = b.build_int_const(0u64, NodeOutputType::I8).unwrap();
        b.build_lzcount(v, NodeOutputType::I8)
    })?;
    assert!(ConstantFold.optimize(&mut fg)?.changed());
    assert_eq!(return_kind(&fg)?, NodeKind::IntConst(8));
    Ok(())
}

#[test]
fn fold_lzcount_zero_u64() -> Result<()> {
    // I64 happened to work on the unfixed code (64 - 64 = 0 shift), but pin
    // it with a regression test so the fix doesn't break it.
    let mut fg = make_fn(|b| {
        let v = b.build_int_const(0u64, NodeOutputType::I64).unwrap();
        b.build_lzcount(v, NodeOutputType::I64)
    })?;
    assert!(ConstantFold.optimize(&mut fg)?.changed());
    assert_eq!(return_kind(&fg)?, NodeKind::IntConst(64));
    Ok(())
}

/// Builds a function whose Return value is `kind(wide_const)`, where the
/// wide constant has declared type `wide_ty` (I128 or I256) and `kind` is
/// either `NodeKind::Lzcount` or `NodeKind::Popcount`.
///
/// `FunctionBuilder::build_int_const` rejects I128/I256 by design, and
/// `build_lzcount`/`build_popcount` would coerce through `convert_to_int_if_needed`
/// (truncating the input to I64). So we build the placeholder skeleton with
/// a I64 const + return, then graft the wide const + unary node directly via
/// the lower-level `Graph` mutators (which bypass make_int_const's I256 reject)
/// and rewire the Return.
fn build_unary_with_wide_const_input(
    kind: NodeKind,
    wide_ty: NodeOutputType,
    out_ty: NodeOutputType,
) -> Result<strider_ir::Function> {
    use strider_ir::node::NodeOutputKind;
    let mut fg = make_fn(|b| Ok(b.build_int_const(0u64, NodeOutputType::I64).unwrap()))?;
    let placeholder = return_value(&fg)?;
    let wide_node = fg.create_node(
        NodeKind::IntConst(0xFF),
        [],
        [NodeOutputKind::OutputType(wide_ty)],
    );
    let wide_const = fg.node_outputs_exact::<1>(wide_node)?[0];
    let unary_node = fg.create_node(
        kind,
        [wide_const],
        [NodeOutputKind::OutputType(out_ty)],
    );
    let unary_out = fg.node_outputs_exact::<1>(unary_node)?[0];
    fg.replace_all_uses(placeholder, unary_out)?;
    Ok(fg)
}

#[test]
fn fold_lzcount_u128_input_skips_cleanly() -> Result<()> {
    // Lzcount on a I128 IntConst must not propagate ExpectedIntegerType — the
    // fold can't compute leading-zeros for a width that doesn't fit u64, so
    // the rule should silently skip (Error::skip) rather than crash the
    // whole optimizer pipeline.
    let mut fg = build_unary_with_wide_const_input(
        NodeKind::Lzcount,
        NodeOutputType::I128,
        NodeOutputType::I64,
    )?;
    let result = ConstantFold.optimize(&mut fg);
    assert!(
        result.is_ok(),
        "ConstantFold must not error on Lzcount(I128 const), got {:?}",
        result.err(),
    );
    Ok(())
}

#[test]
fn fold_lzcount_u256_input_skips_cleanly() -> Result<()> {
    let mut fg = build_unary_with_wide_const_input(
        NodeKind::Lzcount,
        NodeOutputType::I256,
        NodeOutputType::I64,
    )?;
    let result = ConstantFold.optimize(&mut fg);
    assert!(
        result.is_ok(),
        "ConstantFold must not error on Lzcount(I256 const), got {:?}",
        result.err(),
    );
    Ok(())
}

#[test]
fn fold_popcount_u128_input_skips_cleanly() -> Result<()> {
    // Popcount on a I128 IntConst has the same shape: the masking step
    // (get_unsigned_int(I128, _) == None) must trigger a skip, not propagate
    // ExpectedIntegerType up through the pipeline.
    let mut fg = build_unary_with_wide_const_input(
        NodeKind::Popcount,
        NodeOutputType::I128,
        NodeOutputType::I64,
    )?;
    let result = ConstantFold.optimize(&mut fg);
    assert!(
        result.is_ok(),
        "ConstantFold must not error on Popcount(I128 const), got {:?}",
        result.err(),
    );
    Ok(())
}

#[test]
fn fold_popcount_u256_input_skips_cleanly() -> Result<()> {
    let mut fg = build_unary_with_wide_const_input(
        NodeKind::Popcount,
        NodeOutputType::I256,
        NodeOutputType::I64,
    )?;
    let result = ConstantFold.optimize(&mut fg);
    assert!(
        result.is_ok(),
        "ConstantFold must not error on Popcount(I256 const), got {:?}",
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
    assert!(ConstantFold.optimize(&mut fg)?.changed());
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
    assert!(ConstantFold.optimize(&mut fg)?.changed());
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
    assert!(ConstantFold.optimize(&mut fg)?.changed());
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
    assert!(ConstantFold.optimize(&mut fg)?.changed());
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
    assert!(ConstantFold.optimize(&mut fg)?.changed());
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
    assert!(ConstantFold.optimize(&mut fg)?.changed());
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
    assert!(ConstantFold.optimize(&mut fg)?.changed());
    assert_eq!(return_kind(&fg)?, NodeKind::IntConst(1));
    Ok(())
}

#[test]
fn fold_f64_equal_true() -> Result<()> {
    let mut fg = make_fn(|b| {
        let a = b.build_float_const(4.0f64.to_bits(), NodeOutputType::F64);
        let c = b.build_float_const(4.0f64.to_bits(), NodeOutputType::F64);
        b.build_float_cmp_op(a, c, FloatCmpOp::Equal)
    })?;
    assert!(ConstantFold.optimize(&mut fg)?.changed());
    assert_eq!(return_kind(&fg)?, NodeKind::IntConst(1));
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
    assert!(ConstantFold.optimize(&mut fg)?.changed());
    assert_eq!(return_kind(&fg)?, NodeKind::IntConst(0));
    Ok(())
}

#[test]
fn fold_f32_neg_const() -> Result<()> {
    let mut fg = make_fn(|b| {
        let v = b.build_float_const(2.0f32.to_bits() as u64, NodeOutputType::F32);
        b.build_float_unary_op(v, FloatUnaryOp::Neg, NodeOutputType::F32)
    })?;
    assert!(ConstantFold.optimize(&mut fg)?.changed());
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
    assert!(ConstantFold.optimize(&mut fg)?.changed());
    assert_eq!(return_kind(&fg)?, NodeKind::FloatConst(3.0f64.to_bits()));
    Ok(())
}

#[test]
fn fold_f64_sqrt_const() -> Result<()> {
    let mut fg = make_fn(|b| {
        let v = b.build_float_const(4.0f64.to_bits(), NodeOutputType::F64);
        b.build_float_unary_op(v, FloatUnaryOp::Sqrt, NodeOutputType::F64)
    })?;
    assert!(ConstantFold.optimize(&mut fg)?.changed());
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
    assert!(ConstantFold.optimize(&mut fg)?.changed());
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
    assert!(ConstantFold.optimize(&mut fg)?.changed());
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
        assert!(ConstantFold.optimize(&mut fg)?.changed(),
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
        assert!(ConstantFold.optimize(&mut fg)?.changed(),
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
        let as_int = b.build_float_bits_to_int(sum, NodeOutputType::I64)?;
        let back_to_float = b.build_int_bits_to_float(as_int, NodeOutputType::F64)?;
        Ok(back_to_float)
    })?;
    assert!(ConstantFold.optimize(&mut fg)?.changed());
    // Float binary fold: sum → FloatConst(3.0).
    // Bitcast identity fold: IntBitsToFloat(FloatBitsToInt(FloatConst(3.0))) → FloatConst(3.0).
    assert_eq!(return_kind(&fg)?, NodeKind::FloatConst(3.0f64.to_bits()));
    Ok(())
}

// (CastToFloat lowering tests removed: there is no CastToFloat node — an
// int→float cast is built directly as IntBitsToFloat / FloatConst, and a
// float→float cast as FloatToFloat, by `cast_to_float_if_needed` at build
// time.  That lowering is covered by the strider-ir builder tests.)

// ── Comprehensive tests ──────────────────────────────────────────────────────

/// Shift constant evaluation: `1 << 4` for I32 → 0x10.
#[test]
fn fold_shl_const_u32() -> Result<()> {
    let mut fg = make_fn(|b| {
        let x = b.build_int_const(1u64, NodeOutputType::I32).unwrap();
        let n = b.build_int_const(4u64, NodeOutputType::I32).unwrap();
        b.build_int_binary_operation(x, n, IntBinaryOp::ShiftLeft, NodeOutputType::I32)
    })?;
    assert!(ConstantFold.optimize(&mut fg)?.changed());
    assert_eq!(return_kind(&fg)?, NodeKind::IntConst(0x10));
    Ok(())
}

/// Shift at width boundary: `1 << 31` for I32 → 0x80000000.
#[test]
fn fold_shl_at_width_boundary_u32() -> Result<()> {
    let mut fg = make_fn(|b| {
        let x = b.build_int_const(1u64, NodeOutputType::I32).unwrap();
        let n = b.build_int_const(31u64, NodeOutputType::I32).unwrap();
        b.build_int_binary_operation(x, n, IntBinaryOp::ShiftLeft, NodeOutputType::I32)
    })?;
    assert!(ConstantFold.optimize(&mut fg)?.changed());
    assert_eq!(return_kind(&fg)?, NodeKind::IntConst(0x8000_0000));
    Ok(())
}

/// Shift right: `0x80 >> 7` for I8 → 1.
#[test]
fn fold_shr_const_u8() -> Result<()> {
    let mut fg = make_fn(|b| {
        let x = b.build_int_const(0x80u64, NodeOutputType::I8).unwrap();
        let n = b.build_int_const(7u64, NodeOutputType::I8).unwrap();
        b.build_int_binary_operation(x, n, IntBinaryOp::ShiftRight, NodeOutputType::I8)
    })?;
    assert!(ConstantFold.optimize(&mut fg)?.changed());
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
    assert!(ConstantFold.optimize(&mut fg)?.changed());
    let val = return_value(&fg)?;
    if let NodeKind::FloatConst(bits) = *fg.kind_of_output(val) {
        assert!(f64::from_bits(bits).is_nan(), "NaN must propagate through Add");
    } else {
        return Err(anyhow!("assertion failed: expected FloatConst result"));
    }
    Ok(())
}

/// `inf - inf` is NaN per IEEE 754.
#[test]
fn fold_f64_inf_minus_inf_is_nan() -> Result<()> {
    // `inf - inf` lowered to `Add(inf, Neg(inf))`.  Both `Neg(inf)` and
    // the resulting `Add(inf, -inf)` are constant-foldable: `Neg(inf) = -inf`
    // (sign-bit flip), then `inf + (-inf)` is NaN per IEEE 754.
    let inf = f64::INFINITY.to_bits();
    let mut fg = make_fn(|b| {
        let a = b.build_float_const(inf, NodeOutputType::F64);
        let bb = b.build_float_const(inf, NodeOutputType::F64);
        let neg_b = b.build_float_unary_op(bb, FloatUnaryOp::Neg, NodeOutputType::F64)?;
        b.build_float_binary_op(a, neg_b, FloatBinaryOp::Add, NodeOutputType::F64)
    })?;
    assert!(ConstantFold.optimize(&mut fg)?.changed());
    let val = return_value(&fg)?;
    if let NodeKind::FloatConst(bits) = *fg.kind_of_output(val) {
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
        let as_int = b.build_float_bits_to_int(sum, NodeOutputType::I32)?;
        b.build_int_bits_to_float(as_int, NodeOutputType::F32)
    })?;
    assert!(ConstantFold.optimize(&mut fg)?.changed());
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
        let one = b.build_int_const(1u64, NodeOutputType::I32).unwrap();
        let two = b.build_int_const(2u64, NodeOutputType::I32).unwrap();
        let three = b.build_int_const(3u64, NodeOutputType::I32).unwrap();
        let four = b.build_int_const(4u64, NodeOutputType::I32).unwrap();
        let c1 = b.build_int_binary_operation(one, two, IntBinaryOp::Add, NodeOutputType::I32)?;
        let c2 = b.build_int_binary_operation(c1, three, IntBinaryOp::Add, NodeOutputType::I32)?;
        b.build_int_binary_operation(c2, four, IntBinaryOp::Add, NodeOutputType::I32)
    })?;

    // Single optimize() call — must converge without the outer pipeline loop.
    ConstantFold.optimize(&mut fg)?;

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
            let one = b.build_int_const(1u64, NodeOutputType::I64).unwrap();
            acc = b.build_sub_as_add_neg(acc, one, NodeOutputType::I64)?;
        }
        Ok(acc)
    })?;
    let mut changed = true;
    while changed {
        changed = ConstantFold.optimize(&mut fg)?.changed();
    }
    assert_sub_with_const(&fg, x, 10, NodeOutputType::I64)?;
    Ok(())
}

#[test]
fn sdiv_narrow_int_min_neg_one_skips() {
    use crate::opt::constant_fold::eval_int::eval_int_binary;
    use strider_ir::IntBinaryOp;
    use strider_ir::node::NodeOutputType;

    // i32::MIN as u32, then masked to u64. Same shape as the u64 case
    // already guarded explicitly; should also return None.
    assert_eq!(
        eval_int_binary(IntBinaryOp::Sdiv, 0x8000_0000, 0xFFFF_FFFF, NodeOutputType::I32),
        None,
        "Sdiv(i32::MIN, -1) on I32 must skip — signed overflow"
    );
    // i16::MIN, -1 on I16.
    assert_eq!(
        eval_int_binary(IntBinaryOp::Sdiv, 0x8000, 0xFFFF, NodeOutputType::I16),
        None,
        "Sdiv(i16::MIN, -1) on I16 must skip — signed overflow"
    );
    // i8::MIN, -1 on I8.
    assert_eq!(
        eval_int_binary(IntBinaryOp::Sdiv, 0x80, 0xFF, NodeOutputType::I8),
        None,
        "Sdiv(i8::MIN, -1) on I8 must skip — signed overflow"
    );
}

#[test]
fn srem_narrow_int_min_neg_one_skips() {
    use crate::opt::constant_fold::eval_int::eval_int_binary;
    use strider_ir::IntBinaryOp;
    use strider_ir::node::NodeOutputType;

    // Same INT_MIN/-1 case for Srem on every narrow signed type.
    assert_eq!(
        eval_int_binary(IntBinaryOp::Srem, 0x8000_0000, 0xFFFF_FFFF, NodeOutputType::I32),
        None,
        "Srem(i32::MIN, -1) on I32 must skip"
    );
    assert_eq!(
        eval_int_binary(IntBinaryOp::Srem, 0x8000, 0xFFFF, NodeOutputType::I16),
        None,
    );
    assert_eq!(
        eval_int_binary(IntBinaryOp::Srem, 0x80, 0xFF, NodeOutputType::I8),
        None,
    );
}

#[test]
fn eval_int_binary_unsigned_div_unmasked_u8() {
    use crate::opt::constant_fold::eval_int::eval_int_binary;
    use strider_ir::IntBinaryOp;
    use strider_ir::node::NodeOutputType;

    // I8 Div with l carrying high garbage bits beyond I8.
    // Masked: 0xFF / 2 = 0x7F. Unmasked-eval: 0x1FF / 2 = 0xFF (wrong).
    assert_eq!(
        eval_int_binary(IntBinaryOp::Div, 0x1FF, 2, NodeOutputType::I8),
        Some(0x7F),
        "Div must mask inputs to I8 before division"
    );
}

#[test]
fn eval_int_binary_unsigned_rem_unmasked_u16() {
    use crate::opt::constant_fold::eval_int::eval_int_binary;
    use strider_ir::IntBinaryOp;
    use strider_ir::node::NodeOutputType;

    // Masked: 0xFFFF % 0x10 = 0x0F. Unmasked-eval: 0x1FFFF % 0x10 = 0x0F.
    // Pick a divisor that distinguishes: 0xFFFF % 7 = 1, 0x1FFFF % 7 = 5.
    assert_eq!(
        eval_int_binary(IntBinaryOp::Rem, 0x1FFFF, 7, NodeOutputType::I16),
        Some(1),
        "Rem must mask inputs to I16 before remainder"
    );
}

#[test]
fn eval_int_binary_unsigned_shr_unmasked_u8() {
    use crate::opt::constant_fold::eval_int::eval_int_binary;
    use strider_ir::IntBinaryOp;
    use strider_ir::node::NodeOutputType;

    // Masked: 0xFF >> 1 = 0x7F. Unmasked-eval: 0x1FF >> 1 = 0xFF, masked = 0xFF.
    assert_eq!(
        eval_int_binary(IntBinaryOp::ShiftRight, 0x1FF, 1, NodeOutputType::I8),
        Some(0x7F),
        "ShiftRight must mask the input to I8 before shifting"
    );
}

#[test]
fn eval_int_cmp_equal_unmasked_u8() {
    use crate::opt::constant_fold::eval_int::eval_int_cmp;
    use strider_ir::IntCmpOp;
    use strider_ir::node::NodeOutputType;

    // Masked: 0xFF == 0xFF → true. Unmasked-eval: 0x1FF != 0xFF → false.
    assert!(
        eval_int_cmp(IntCmpOp::Equal, 0x1FF, 0xFF, NodeOutputType::I8).unwrap(),
        "Equal must mask both sides to I8 before comparing"
    );
}

#[test]
fn eval_int_cmp_less_unmasked_u8() {
    use crate::opt::constant_fold::eval_int::eval_int_cmp;
    use strider_ir::IntCmpOp;
    use strider_ir::node::NodeOutputType;

    // Masked: 0x00 < 0x01 → true. Unmasked-eval: 0x100 < 0x01 → false.
    assert!(
        eval_int_cmp(IntCmpOp::Less, 0x100, 0x01, NodeOutputType::I8).unwrap(),
        "Less must mask both sides to I8 before comparing"
    );
}

#[test]
fn eval_int_cmp_carry_unmasked_u8() {
    use crate::opt::constant_fold::eval_int::eval_int_cmp;
    use strider_ir::IntCmpOp;
    use strider_ir::node::NodeOutputType;

    // Masked: 0x00 + 0x00 → no carry. Unmasked-eval: 0x100 + 0 = 0x100 > 0xFF → false-carry.
    assert!(
        !eval_int_cmp(IntCmpOp::Carry, 0x100, 0, NodeOutputType::I8).unwrap(),
        "Carry must mask both sides before checking overflow"
    );
}

// ── Bitwise-complement and two's-complement constant-fold semantics ────
//
// Bitwise complement (`~x`) is `Xor(x, all_ones)` — the canonical IR
// shape since the former BitNot unary-op was removed.  `Neg` is two's-complement
// negation (`-x`).  The MVN-based ARM `if_returns_const` lowering
// produces `Xor(IntConst(49), IntConst(all_ones))` which must fold to
// `~49 = -50`, not to `wrapping_neg(49) = -49`.

/// `Xor(IntConst(49), IntConst(all_ones))` at I32 must fold to `~49`
/// (= 0xFFFF_FFCE = 4_294_967_246) — bitwise NOT, NOT two's complement.
#[test]
fn fold_int_unary_neg_is_bitwise_not_u32() -> Result<()> {
    let mut fg = make_fn(|b| {
        let c = b.build_int_const(49u64, NodeOutputType::I32).unwrap();
        let one = b.build_all_ones_const(NodeOutputType::I32)?;
        b.build_int_binary_operation(c, one, IntBinaryOp::Xor, NodeOutputType::I32)
    })?;
    assert!(ConstantFold.optimize(&mut fg)?.changed());
    assert_eq!(
        return_kind(&fg)?,
        NodeKind::IntConst(0xFFFF_FFCE),
        "Xor(49, ~0) at I32 must fold to bitwise NOT (=~49=0xFFFFFFCE)"
    );
    Ok(())
}

/// `IntUnaryOp::Neg` of `IntConst(50)` at I32 must fold to `-50`
/// (= 0xFFFF_FFCE = 4_294_967_246) — two's complement, NOT bitwise NOT.
#[test]
fn fold_int_unary_not_is_two_complement_u32() -> Result<()> {
    let mut fg = make_fn(|b| {
        let c = b.build_int_const(50u64, NodeOutputType::I32).unwrap();
        b.build_int_unary_operation(c, IntUnaryOp::Neg, NodeOutputType::I32)
    })?;
    assert!(ConstantFold.optimize(&mut fg)?.changed());
    assert_eq!(
        return_kind(&fg)?,
        NodeKind::IntConst(0xFFFF_FFCE),
        "IntUnaryOp::Neg(50) must fold to two's complement (=-50=0xFFFFFFCE), \
         not bitwise NOT (=~50=0xFFFFFFCD)"
    );
    Ok(())
}

/// At I8: `Xor(0xAA, 0xFF)` must fold to `~0xAA = 0x55` (bitwise NOT).
#[test]
fn fold_int_unary_neg_intermediate_is_bitwise_not_u8() -> Result<()> {
    let mut fg = make_fn(|b| {
        let c = b.build_int_const(0xAAu64, NodeOutputType::I8).unwrap();
        let one = b.build_all_ones_const(NodeOutputType::I8)?;
        b.build_int_binary_operation(c, one, IntBinaryOp::Xor, NodeOutputType::I8)
    })?;
    assert!(ConstantFold.optimize(&mut fg)?.changed());
    assert_eq!(
        return_kind(&fg)?,
        NodeKind::IntConst(0x55),
        "Xor(0xAA, 0xFF) at I8 must be ~0xAA = 0x55 (bitwise NOT)"
    );
    Ok(())
}

/// Two's complement of 0 is 0 — sanity check for the I64 path.
#[test]
fn fold_int_unary_not_zero_is_zero() -> Result<()> {
    let mut fg = make_fn(|b| {
        let c = b.build_int_const(0u64, NodeOutputType::I64).unwrap();
        b.build_int_unary_operation(c, IntUnaryOp::Neg, NodeOutputType::I64)
    })?;
    assert!(ConstantFold.optimize(&mut fg)?.changed());
    assert_eq!(return_kind(&fg)?, NodeKind::IntConst(0));
    Ok(())
}

/// Bitwise NOT of 0 is all-ones at the type width — `Xor(0, ~0) = ~0`.
#[test]
fn fold_int_unary_neg_zero_is_all_ones_u32() -> Result<()> {
    let mut fg = make_fn(|b| {
        let c = b.build_int_const(0u64, NodeOutputType::I32).unwrap();
        let one = b.build_all_ones_const(NodeOutputType::I32)?;
        b.build_int_binary_operation(c, one, IntBinaryOp::Xor, NodeOutputType::I32)
    })?;
    assert!(ConstantFold.optimize(&mut fg)?.changed());
    assert_eq!(
        return_kind(&fg)?,
        NodeKind::IntConst(0xFFFF_FFFF),
        "Xor(0, ~0) at I32 must be 0xFFFFFFFF (bitwise NOT of 0)"
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
// which gives `1 << (32 % 32) = 1 << 0 = 1` for `IntConst(1, I32) << 32`
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
    use crate::opt::constant_fold::eval_int::eval_int_binary;

    assert_eq!(
        eval_int_binary(IntBinaryOp::ShiftLeft, 1, 32, NodeOutputType::I32),
        Some(0),
        "Sleigh: 1u32 << 32 = 0 (`r >= 8*sizeout` returns 0 per opbehavior.cc:411). \
         Pre-fix fold computed `1 << (32 % 32) = 1 << 0 = 1` — diverges from Sleigh."
    );
}

/// Sleigh `INT_LEFT(IntConst(1), IntConst(64))` at sizeout=8 evaluates
/// to 0.  At u64 the wider type doesn't change the rule.
#[test]
fn eval_int_binary_shl_at_bit_width_returns_zero_u64() {
    use crate::opt::constant_fold::eval_int::eval_int_binary;

    assert_eq!(
        eval_int_binary(IntBinaryOp::ShiftLeft, 1, 64, NodeOutputType::I64),
        Some(0),
        "Sleigh: 1u64 << 64 = 0.  Pre-fix fold computed `1 << (64 % 64) = 1`."
    );
}

/// Sleigh `INT_LEFT(IntConst(0xFF), IntConst(40))` at sizeout=4 evaluates
/// to 0 (40 > 32).  Beyond-bit-width shifts also zero the result.
#[test]
fn eval_int_binary_shl_above_bit_width_returns_zero_u32() {
    use crate::opt::constant_fold::eval_int::eval_int_binary;

    assert_eq!(
        eval_int_binary(IntBinaryOp::ShiftLeft, 0xFF, 40, NodeOutputType::I32),
        Some(0),
        "Sleigh: shift > bit-width still returns 0.  Pre-fix fold computed \
         `0xFF << (40 % 32) = 0xFF << 8 = 0xFF00`."
    );
}

/// Sleigh `INT_RIGHT(IntConst(0xFFFF_FFFF), IntConst(32))` at sizeout=4
/// evaluates to 0 — same out-of-range rule as INT_LEFT.
#[test]
fn eval_int_binary_shr_at_bit_width_returns_zero_u32() {
    use crate::opt::constant_fold::eval_int::eval_int_binary;

    assert_eq!(
        eval_int_binary(IntBinaryOp::ShiftRight, 0xFFFF_FFFF, 32, NodeOutputType::I32),
        Some(0),
        "Sleigh: 0xFFFFFFFFu32 >> 32 = 0 per opbehavior.cc:432.  Pre-fix \
         fold computed `0xFFFFFFFF >> (32 % 32) = 0xFFFFFFFF`."
    );
}

/// Sleigh `INT_SRIGHT(IntConst(0xFFFF_FFFF), IntConst(32))` at sizeout=4
/// evaluates to 0xFFFF_FFFF (sign bit set → fill with all-ones).
#[test]
fn eval_int_binary_sshr_at_bit_width_negative_returns_all_ones_u32() {
    use crate::opt::constant_fold::eval_int::eval_int_binary;

    assert_eq!(
        eval_int_binary(IntBinaryOp::SShiftRight, 0xFFFF_FFFF, 32, NodeOutputType::I32),
        Some(0xFFFF_FFFF),
        "Sleigh: signed-negative i32::MAX-style >> 32 fills with sign bit \
         (= 0xFFFFFFFF) per opbehavior.cc:454-460."
    );
}

/// Sleigh `INT_SRIGHT(IntConst(0x7FFF_FFFF), IntConst(32))` at sizeout=4
/// evaluates to 0 (sign bit clear → fill with zeros).
#[test]
fn eval_int_binary_sshr_at_bit_width_positive_returns_zero_u32() {
    use crate::opt::constant_fold::eval_int::eval_int_binary;

    assert_eq!(
        eval_int_binary(IntBinaryOp::SShiftRight, 0x7FFF_FFFF, 32, NodeOutputType::I32),
        Some(0),
        "Sleigh: signed-non-negative >> bit-width = 0 (no sign bit to fill)."
    );
}

