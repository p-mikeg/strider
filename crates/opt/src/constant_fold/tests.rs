use super::*;
use crate::error::ErrorKind;
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
    let mut b = FunctionBuilder::new(vec![], &[], &[], &[])?;
    let region = b.create_region()?;
    b.set_entry_region(region)?;
    b.set_region(region);
    let val = f(&mut b)?;
    b.build_return(Some(val), &[])?;
    Ok(b.build()?)
}

/// Returns the output id that the Return node receives as its value
/// argument (input[1]: input[0] is the control edge).
fn return_value(fg: &ir::BuiltFunctionGraph) -> Result<ir::Value> {
    let ret = fg
        .all_node_ids()
        .find(|&n| matches!(fg.graph.node_kind(n), NodeKind::Return))
        .ok_or(ErrorKind::NoReturnNode)?;
    Ok(fg.graph.node_inputs(ret)[1])
}

/// Returns the `NodeKind` of the node that produces the return value.
fn return_kind(fg: &ir::BuiltFunctionGraph) -> Result<NodeKind> {
    let val = return_value(fg)?;
    let node = fg.graph.get_node_from_output(val);
    Ok(*fg.graph.node_kind(node))
}

// ── integer binary folding ────────────────────────────────────────────────

#[test]
fn fold_int_add_consts() -> Result<()> {
    let mut fg = make_fn(|b| {
        let c3 = b.build_int_const(3, NodeOutputType::U64);
        let c4 = b.build_int_const(4, NodeOutputType::U64);
        Ok(b.build_int_binary_operation(c3, c4, IntBinaryOp::Add, NodeOutputType::U64)?)
    })?;
    assert!(ConstantFold.optimize(&mut fg)?.changed());
    assert_eq!(return_kind(&fg)?, NodeKind::IntConst(7));
    Ok(())
}

#[test]
fn fold_int_and_zero() -> Result<()> {
    let mut fg = make_fn(|b| {
        let x = b.build_int_const(0xFF, NodeOutputType::U64);
        let zero = b.build_int_const(0, NodeOutputType::U64);
        Ok(b.build_int_binary_operation(x, zero, IntBinaryOp::And, NodeOutputType::U64)?)
    })?;
    assert!(ConstantFold.optimize(&mut fg)?.changed());
    assert_eq!(return_kind(&fg)?, NodeKind::IntConst(0));
    Ok(())
}

#[test]
fn fold_int_xor_self() -> Result<()> {
    let mut fg = make_fn(|b| {
        let x = b.build_int_const(0xAB, NodeOutputType::U64);
        Ok(b.build_int_binary_operation(x, x, IntBinaryOp::Xor, NodeOutputType::U64)?)
    })?;
    assert!(ConstantFold.optimize(&mut fg)?.changed());
    assert_eq!(return_kind(&fg)?, NodeKind::IntConst(0));
    Ok(())
}

#[test]
fn fold_int_sub_self() -> Result<()> {
    let mut fg = make_fn(|b| {
        let x = b.build_int_const(0xAB, NodeOutputType::U64);
        Ok(b.build_int_binary_operation(x, x, IntBinaryOp::Sub, NodeOutputType::U64)?)
    })?;
    assert!(ConstantFold.optimize(&mut fg)?.changed());
    assert_eq!(return_kind(&fg)?, NodeKind::IntConst(0));
    Ok(())
}

#[test]
fn fold_add_zero_identity() -> Result<()> {
    // x + 0 → x  (x is non-const)
    let mut fg = make_fn(|b| {
        let c1 = b.build_int_const(1, NodeOutputType::U64);
        let c2 = b.build_int_const(2, NodeOutputType::U64);
        let x = b.build_int_binary_operation(c1, c2, IntBinaryOp::Add, NodeOutputType::U64)?;
        let zero = b.build_int_const(0, NodeOutputType::U64);
        Ok(b.build_int_binary_operation(x, zero, IntBinaryOp::Add, NodeOutputType::U64)?)
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
        let c5 = b.build_int_const(5, NodeOutputType::U64);
        let one = b.build_int_const(1, NodeOutputType::U64);
        Ok(b.build_int_binary_operation(c5, one, IntBinaryOp::Mul, NodeOutputType::U64)?)
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
        let x = b.build_int_const(0xFF, NodeOutputType::U64);
        let c4 = b.build_int_const(4, NodeOutputType::U64);
        let c7 = b.build_int_const(7, NodeOutputType::U64);
        let inner =
            b.build_int_binary_operation(x, c4, IntBinaryOp::And, NodeOutputType::U64)?;
        Ok(b.build_int_binary_operation(inner, c7, IntBinaryOp::And, NodeOutputType::U64)?)
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
    let mut b = FunctionBuilder::new(vec![vn], &[vn], &[], &[])?;
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
        .ok_or(ErrorKind::ExpectedIntegerType(ty))?;
    let const_on = |o: ir::Value| -> bool {
        matches!(
            *fg.graph.node_kind(fg.graph.get_node_from_output(o)),
            NodeKind::IntConst(v) if ty.get_unsigned_int(v) == Some(masked)
        )
    };
    let ok = (l == expected_base && const_on(r)) || (r == expected_base && const_on(l));
    assert!(
        ok,
        "expected `base + {:#x}`; got lhs kind={:?}, rhs kind={:?}",
        masked,
        fg.graph.node_kind(fg.graph.get_node_from_output(l)),
        fg.graph.node_kind(fg.graph.get_node_from_output(r)),
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
        .ok_or(ErrorKind::ExpectedIntegerType(ty))?;
    let const_on_rhs = matches!(
        *fg.graph.node_kind(fg.graph.get_node_from_output(r)),
        NodeKind::IntConst(v) if ty.get_unsigned_int(v) == Some(masked)
    );
    assert!(
        l == expected_base && const_on_rhs,
        "expected `base - {:#x}`; got lhs kind={:?}, rhs kind={:?}",
        masked,
        fg.graph.node_kind(fg.graph.get_node_from_output(l)),
        fg.graph.node_kind(fg.graph.get_node_from_output(r)),
    );
    Ok(())
}

#[test]
fn reassoc_add_add_consts() -> Result<()> {
    // (x + 3) + 4 → x + 7
    let vn = reg_vn(0x1000, 8);
    let (mut fg, x) = make_fn_with_var(vn, |b, x| {
        let c3 = b.build_int_const(3, NodeOutputType::U64);
        let c4 = b.build_int_const(4, NodeOutputType::U64);
        let inner =
            b.build_int_binary_operation(x, c3, IntBinaryOp::Add, NodeOutputType::U64)?;
        Ok(b.build_int_binary_operation(inner, c4, IntBinaryOp::Add, NodeOutputType::U64)?)
    })?;
    let mut changed = true;
    while changed {
        changed = ConstantFold.optimize(&mut fg)?.changed();
    }
    assert_add_with_const(&fg, x, 7, NodeOutputType::U64)?;
    Ok(())
}

#[test]
fn reassoc_add_sub_consts() -> Result<()> {
    // (x - 3) + 4 → x + 1
    let vn = reg_vn(0x1000, 8);
    let (mut fg, x) = make_fn_with_var(vn, |b, x| {
        let c3 = b.build_int_const(3, NodeOutputType::U64);
        let c4 = b.build_int_const(4, NodeOutputType::U64);
        let inner =
            b.build_int_binary_operation(x, c3, IntBinaryOp::Sub, NodeOutputType::U64)?;
        Ok(b.build_int_binary_operation(inner, c4, IntBinaryOp::Add, NodeOutputType::U64)?)
    })?;
    let mut changed = true;
    while changed {
        changed = ConstantFold.optimize(&mut fg)?.changed();
    }
    assert_add_with_const(&fg, x, 1, NodeOutputType::U64)?;
    Ok(())
}

#[test]
fn reassoc_sub_add_consts_wrapping() -> Result<()> {
    // (x + 3) - 4 → x + (3 - 4)  = x + 0xFFFF_FFFF_FFFF_FFFF
    let vn = reg_vn(0x1000, 8);
    let (mut fg, x) = make_fn_with_var(vn, |b, x| {
        let c3 = b.build_int_const(3, NodeOutputType::U64);
        let c4 = b.build_int_const(4, NodeOutputType::U64);
        let inner =
            b.build_int_binary_operation(x, c3, IntBinaryOp::Add, NodeOutputType::U64)?;
        Ok(b.build_int_binary_operation(inner, c4, IntBinaryOp::Sub, NodeOutputType::U64)?)
    })?;
    let mut changed = true;
    while changed {
        changed = ConstantFold.optimize(&mut fg)?.changed();
    }
    assert_add_with_const(&fg, x, 0xFFFF_FFFF_FFFF_FFFF, NodeOutputType::U64)?;
    Ok(())
}

#[test]
fn reassoc_sub_sub_consts() -> Result<()> {
    // (x - 3) - 4 → x - 7
    let vn = reg_vn(0x1000, 8);
    let (mut fg, x) = make_fn_with_var(vn, |b, x| {
        let c3 = b.build_int_const(3, NodeOutputType::U64);
        let c4 = b.build_int_const(4, NodeOutputType::U64);
        let inner =
            b.build_int_binary_operation(x, c3, IntBinaryOp::Sub, NodeOutputType::U64)?;
        Ok(b.build_int_binary_operation(inner, c4, IntBinaryOp::Sub, NodeOutputType::U64)?)
    })?;
    let mut changed = true;
    while changed {
        changed = ConstantFold.optimize(&mut fg)?.changed();
    }
    assert_sub_with_const(&fg, x, 7, NodeOutputType::U64)?;
    Ok(())
}

#[test]
fn reassoc_add_commuted_inner() -> Result<()> {
    // (3 + x) + 4 → x + 7 (inner Add has const on lhs)
    let vn = reg_vn(0x1000, 8);
    let (mut fg, x) = make_fn_with_var(vn, |b, x| {
        let c3 = b.build_int_const(3, NodeOutputType::U64);
        let c4 = b.build_int_const(4, NodeOutputType::U64);
        let inner =
            b.build_int_binary_operation(c3, x, IntBinaryOp::Add, NodeOutputType::U64)?;
        Ok(b.build_int_binary_operation(inner, c4, IntBinaryOp::Add, NodeOutputType::U64)?)
    })?;
    let mut changed = true;
    while changed {
        changed = ConstantFold.optimize(&mut fg)?.changed();
    }
    assert_add_with_const(&fg, x, 7, NodeOutputType::U64)?;
    Ok(())
}

#[test]
fn reassoc_add_commuted_outer() -> Result<()> {
    // 4 + (x + 3) → x + 7 (outer Add has const on lhs)
    let vn = reg_vn(0x1000, 8);
    let (mut fg, x) = make_fn_with_var(vn, |b, x| {
        let c3 = b.build_int_const(3, NodeOutputType::U64);
        let c4 = b.build_int_const(4, NodeOutputType::U64);
        let inner =
            b.build_int_binary_operation(x, c3, IntBinaryOp::Add, NodeOutputType::U64)?;
        Ok(b.build_int_binary_operation(c4, inner, IntBinaryOp::Add, NodeOutputType::U64)?)
    })?;
    let mut changed = true;
    while changed {
        changed = ConstantFold.optimize(&mut fg)?.changed();
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
        let c4 = b.build_int_const(4, NodeOutputType::U64);
        let a = b.build_int_binary_operation(x, c4, IntBinaryOp::Sub, NodeOutputType::U64)?;
        let b_ = b.build_int_binary_operation(a, c4, IntBinaryOp::Sub, NodeOutputType::U64)?;
        Ok(b.build_int_binary_operation(b_, c4, IntBinaryOp::Sub, NodeOutputType::U64)?)
    })?;
    let mut changed = true;
    while changed {
        changed = ConstantFold.optimize(&mut fg)?.changed();
    }
    assert_sub_with_const(&fg, x, 12, NodeOutputType::U64)?;
    Ok(())
}

#[test]
fn reassoc_chain_three_subs_u32() -> Result<()> {
    // Same chain but at U32: ((x - 4) - 4) - 4 → x - 12.
    let vn = reg_vn(0x1000, 4);
    let (mut fg, x) = make_fn_with_var(vn, |b, x| {
        let c4 = b.build_int_const(4, NodeOutputType::U32);
        let a = b.build_int_binary_operation(x, c4, IntBinaryOp::Sub, NodeOutputType::U32)?;
        let b_ = b.build_int_binary_operation(a, c4, IntBinaryOp::Sub, NodeOutputType::U32)?;
        Ok(b.build_int_binary_operation(b_, c4, IntBinaryOp::Sub, NodeOutputType::U32)?)
    })?;
    let mut changed = true;
    while changed {
        changed = ConstantFold.optimize(&mut fg)?.changed();
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
    let mut b = FunctionBuilder::new(vec![xv, yv, zv], &[xv, yv, zv], &[], &[])?;
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
    let mut b = FunctionBuilder::new(vec![av, bv], &[av, bv], &[], &[])?;
    let r = b.create_region()?;
    b.set_entry_region(r)?;
    b.set_region(r);
    let a = b.read_variable(&av)?;
    let bval = b.read_variable(&bv)?;
    let f0 = b.build_int_const(0xF0, NodeOutputType::U64);
    let f0_ = b.build_int_const(0x0F, NodeOutputType::U64);
    let ff = b.build_int_const(0xFF, NodeOutputType::U64);
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
        let wide = b.build_int_const(0xFF00, NodeOutputType::U16);
        Ok(b.truncate_if_needed(wide, NodeOutputType::U8)?)
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

// ── boolean folding ───────────────────────────────────────────────────────

#[test]
fn fold_bool_neg_const() -> Result<()> {
    let mut fg = make_fn(|b| {
        let t = b.build_boolean_const(true);
        Ok(b.build_boolean_unary_operation(t, BoolUnaryOp::Neg)?)
    })?;
    assert!(ConstantFold.optimize(&mut fg)?.changed());
    assert_eq!(return_kind(&fg)?, NodeKind::BoolConst(false));
    Ok(())
}

#[test]
fn fold_bool_and_consts() -> Result<()> {
    let mut fg = make_fn(|b| {
        let t = b.build_boolean_const(true);
        let f = b.build_boolean_const(false);
        Ok(b.build_boolean_operation(t, f, BoolBinaryOp::And)?)
    })?;
    assert!(ConstantFold.optimize(&mut fg)?.changed());
    assert_eq!(return_kind(&fg)?, NodeKind::BoolConst(false));
    Ok(())
}

// ── no-fold edge cases ────────────────────────────────────────────────────

#[test]
fn no_fold_div_by_zero() -> Result<()> {
    let mut fg = make_fn(|b| {
        let x = b.build_int_const(10, NodeOutputType::U64);
        let zero = b.build_int_const(0, NodeOutputType::U64);
        Ok(b.build_int_binary_operation(x, zero, IntBinaryOp::Div, NodeOutputType::U64)?)
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
        let c5 = b.build_int_const(5, NodeOutputType::U64);
        let c5b = b.build_int_const(5, NodeOutputType::U64);
        Ok(b.build_int_cmp_operation(c5, c5b, IntCmpOp::Equal, NodeOutputType::U64)?)
    })?;
    assert!(ConstantFold.optimize(&mut fg)?.changed());
    assert_eq!(return_kind(&fg)?, NodeKind::BoolConst(true));
    Ok(())
}

#[test]
fn fold_int_cmp_less_consts() -> Result<()> {
    let mut fg = make_fn(|b| {
        let c3 = b.build_int_const(3, NodeOutputType::U64);
        let c5 = b.build_int_const(5, NodeOutputType::U64);
        Ok(b.build_int_cmp_operation(c3, c5, IntCmpOp::Less, NodeOutputType::U64)?)
    })?;
    assert!(ConstantFold.optimize(&mut fg)?.changed());
    assert_eq!(return_kind(&fg)?, NodeKind::BoolConst(true));
    Ok(())
}

// ── Popcount / Lzcount ────────────────────────────────────────────────────

#[test]
fn fold_popcount_const() -> Result<()> {
    // popcount(0b10110101) = 5
    let mut fg = make_fn(|b| {
        let v = b.build_int_const(0b10110101, NodeOutputType::U8);
        Ok(b.build_popcount(v, NodeOutputType::U8)?)
    })?;
    assert!(ConstantFold.optimize(&mut fg)?.changed());
    assert_eq!(return_kind(&fg)?, NodeKind::IntConst(5));
    Ok(())
}

#[test]
fn fold_popcount_zero() -> Result<()> {
    let mut fg = make_fn(|b| {
        let v = b.build_int_const(0, NodeOutputType::U64);
        Ok(b.build_popcount(v, NodeOutputType::U64)?)
    })?;
    assert!(ConstantFold.optimize(&mut fg)?.changed());
    assert_eq!(return_kind(&fg)?, NodeKind::IntConst(0));
    Ok(())
}

#[test]
fn fold_lzcount_msb_set() -> Result<()> {
    // lzcount(0x80u8) = 0 (MSB is set)
    let mut fg = make_fn(|b| {
        let v = b.build_int_const(0x80, NodeOutputType::U8);
        Ok(b.build_lzcount(v, NodeOutputType::U8)?)
    })?;
    assert!(ConstantFold.optimize(&mut fg)?.changed());
    assert_eq!(return_kind(&fg)?, NodeKind::IntConst(0));
    Ok(())
}

#[test]
fn fold_lzcount_one() -> Result<()> {
    // lzcount(1u8) = 7 (only bit 0 set in an 8-bit value)
    let mut fg = make_fn(|b| {
        let v = b.build_int_const(1, NodeOutputType::U8);
        Ok(b.build_lzcount(v, NodeOutputType::U8)?)
    })?;
    assert!(ConstantFold.optimize(&mut fg)?.changed());
    assert_eq!(return_kind(&fg)?, NodeKind::IntConst(7));
    Ok(())
}

// ── Float constant folding ────────────────────────────────────────────────

#[test]
fn fold_f32_add_consts() -> Result<()> {
    let mut fg = make_fn(|b| {
        let a = b.build_float_const(3.0f32.to_bits() as u64, NodeOutputType::F32);
        let c = b.build_float_const(4.0f32.to_bits() as u64, NodeOutputType::F32);
        Ok(b.build_float_binary_op(a, c, FloatBinaryOp::Add, NodeOutputType::F32)?)
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
        Ok(b.build_float_binary_op(a, c, FloatBinaryOp::Mul, NodeOutputType::F32)?)
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
        Ok(b.build_float_binary_op(a, c, FloatBinaryOp::Div, NodeOutputType::F32)?)
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
        Ok(b.build_float_binary_op(a, c, FloatBinaryOp::Add, NodeOutputType::F64)?)
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
        Ok(b.build_float_binary_op(a, c, FloatBinaryOp::Mul, NodeOutputType::F64)?)
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
        Ok(b.build_float_binary_op(a, c, FloatBinaryOp::Div, NodeOutputType::F64)?)
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
        Ok(b.build_float_cmp_op(a, c, FloatCmpOp::Less)?)
    })?;
    assert!(ConstantFold.optimize(&mut fg)?.changed());
    assert_eq!(return_kind(&fg)?, NodeKind::BoolConst(true));
    Ok(())
}

#[test]
fn fold_f64_equal_true() -> Result<()> {
    let mut fg = make_fn(|b| {
        let a = b.build_float_const(4.0f64.to_bits(), NodeOutputType::F64);
        let c = b.build_float_const(4.0f64.to_bits(), NodeOutputType::F64);
        Ok(b.build_float_cmp_op(a, c, FloatCmpOp::Equal)?)
    })?;
    assert!(ConstantFold.optimize(&mut fg)?.changed());
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
        Ok(b.build_float_cmp_op(a, c, FloatCmpOp::Equal)?)
    })?;
    assert!(ConstantFold.optimize(&mut fg)?.changed());
    assert_eq!(return_kind(&fg)?, NodeKind::BoolConst(false));
    Ok(())
}

#[test]
fn fold_f32_neg_const() -> Result<()> {
    let mut fg = make_fn(|b| {
        let v = b.build_float_const(2.0f32.to_bits() as u64, NodeOutputType::F32);
        Ok(b.build_float_unary_op(v, FloatUnaryOp::Neg, NodeOutputType::F32)?)
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
        Ok(b.build_float_unary_op(v, FloatUnaryOp::Abs, NodeOutputType::F64)?)
    })?;
    assert!(ConstantFold.optimize(&mut fg)?.changed());
    assert_eq!(return_kind(&fg)?, NodeKind::FloatConst(3.0f64.to_bits()));
    Ok(())
}

#[test]
fn fold_f64_sqrt_const() -> Result<()> {
    let mut fg = make_fn(|b| {
        let v = b.build_float_const(4.0f64.to_bits(), NodeOutputType::F64);
        Ok(b.build_float_unary_op(v, FloatUnaryOp::Sqrt, NodeOutputType::F64)?)
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
        Ok(b.build_float_binary_op(x, one, FloatBinaryOp::Mul, NodeOutputType::F64)?)
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
        Ok(b.build_float_binary_op(x, one, FloatBinaryOp::Div, NodeOutputType::F64)?)
    })?;
    assert!(ConstantFold.optimize(&mut fg)?.changed());
    assert_eq!(return_kind(&fg)?, NodeKind::FloatConst(2.5f64.to_bits()));
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
    assert!(ConstantFold.optimize(&mut fg)?.changed());
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
    assert!(ConstantFold.optimize(&mut fg)?.changed());
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
    assert!(ConstantFold.optimize(&mut fg)?.changed());
    // CastToFloat(F32 → F32) → identity (FloatConst)
    assert_eq!(return_kind(&fg)?, NodeKind::FloatConst(bits));
    Ok(())
}

#[test]
fn cast_to_float_int_non_const_lowers_to_int_bits_to_float() -> Result<()> {
    let mut fg = make_fn(|b| {
        let int_a = b.build_int_const(1, NodeOutputType::U32);
        let int_b = b.build_int_const(2, NodeOutputType::U32);
        // Non-const int (Add result).
        let sum =
            b.build_int_binary_operation(int_a, int_b, IntBinaryOp::Add, NodeOutputType::U32)?;
        let cast = b.build_cast_to_float(sum, NodeOutputType::F32);
        Ok(cast)
    })?;
    assert!(ConstantFold.optimize(&mut fg)?.changed());
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
    assert!(ConstantFold.optimize(&mut fg)?.changed());
    // F32 → F64 should lower to FloatToFloat.
    assert_eq!(return_kind(&fg)?, NodeKind::FloatToFloat);
    Ok(())
}
