use super::*;
use anyhow::anyhow;
use strider_ir::node::{NodeKind, ValueType};
use strider_ir_test_utils::IrWalkerEx;
use strider_ir::{
    FloatBinaryOp, FloatCmpOp, FloatUnaryOp, IRBuilderExt, IRViewer, IRWalker, IntBinaryOp,
    IntCmpOp, IntUnaryOp,
};

use crate::test_support::{
    assert_return_kind, assert_returns_const, make_fn, make_fn_with_var, return_kind, return_value,
    run_to_fixed_point,
};
use strider_ir_test_utils::{RegisterSet, reg_vn};

// ── constructed-with-data: per-instance rule ownership ────────────────────

/// Builds a fixture that folds `3 + 4 → 7` and returns it.
fn add_consts_fixture() -> Result<strider_ir::Function> {
    make_fn(|b| {
        let c3 = b.build_int_const(3u64, ValueType::I64).unwrap();
        let c4 = b.build_int_const(4u64, ValueType::I64).unwrap();
        b.build_int_binary_operation(c3, c4, IntBinaryOp::Add, ValueType::I64)
    })
}

/// Proof-completeness: const-eval folds `c3 + c4 → 7`, so the operand
/// constants are *read* (not reused) and die.  The fresh `IntConst(7)` must
/// absorb both operands' asm-fingerprints — otherwise the asm that produced
/// each operand is lost.  The operands carry distinct addresses from the
/// `Add`, so without the rewrite-engine's interior absorption the result
/// would carry only the `Add`'s address.
#[test]
fn const_eval_absorbs_operand_fingerprints() -> Result<()> {
    const A: u64 = 0xC0FF_EE01;
    const B: u64 = 0xC0FF_EE02;
    const ADD: u64 = 0xC0FF_EE03;
    let mut fg = make_fn(|b| {
        b.set_lift_addr(Some(A));
        let c3 = b.build_int_const(3u64, ValueType::I64).unwrap();
        b.set_lift_addr(Some(B));
        let c4 = b.build_int_const(4u64, ValueType::I64).unwrap();
        b.set_lift_addr(Some(ADD));
        b.build_int_binary_operation(c3, c4, IntBinaryOp::Add, ValueType::I64)
    })?;

    crate::pipeline::run_one(&ConstantFold::new(), &mut fg, &mut crate::OptCtx::new(None))?;

    let folded = return_value(fg.graph())?;
    let fp = fg.asm_fingerprint(fg.producer(folded));
    assert!(
        fp.contains(&A) && fp.contains(&B),
        "const-eval result must absorb both dying operand fingerprints \
         ({A:#x}, {B:#x}); got {fp:?}"
    );
    Ok(())
}

/// A pass built via [`ConstantFold::new`] owns its rule set and folds the
/// same representative constant expression the bare-value form did.
#[test]
fn new_builds_pass_that_folds() -> Result<()> {
    let mut fg = add_consts_fixture()?;
    assert!(
        crate::pipeline::run_one(&ConstantFold::new(), &mut fg, &mut crate::OptCtx::new(None))?
            .changed()
    );
    assert_returns_const(&fg, 7);
    Ok(())
}

/// Two independently-constructed `ConstantFold` instances each own their
/// own rule set — proving the data is per-instance, not a shared
/// thread-local.  Running one then a fresh second on equivalent fixtures
/// both produce the same fold.
#[test]
fn two_independent_instances_each_fold() -> Result<()> {
    let pass_a = ConstantFold::new();
    let pass_b = ConstantFold::new();

    let mut fg_a = add_consts_fixture()?;
    assert!(
        crate::pipeline::run_one(&pass_a, &mut fg_a, &mut crate::OptCtx::new(None))?
            .changed()
    );
    assert_returns_const(&fg_a, 7);

    let mut fg_b = add_consts_fixture()?;
    assert!(
        crate::pipeline::run_one(&pass_b, &mut fg_b, &mut crate::OptCtx::new(None))?
            .changed()
    );
    assert_returns_const(&fg_b, 7);
    Ok(())
}

/// Width-consistency guard (M4): the int-binary const-eval masks both
/// operands to the *output* width.  A synthetic node whose operand widths
/// differ from the output width is not something the lifter emits (it keeps
/// ops width-consistent), and the validator types `IntBinaryOp` inputs only
/// as `AnyInt`, so a mismatch would let the eval mask silently change a
/// value.  The guard must *skip* such a fold rather than emit a possibly
/// wrong constant.
#[test]
fn int_binary_fold_skips_width_mismatched_operands() -> Result<()> {
    use strider_ir::IRBuilder;
    use strider_ir::node::ValueKind;
    // ShiftLeft where the shift-amount operand is wider than the output:
    // masking the amount to the output width changes shift semantics.
    let mut fg = make_fn(|b| {
        let lhs = b.build_int_const(1u64, ValueType::I32)?; // I32
        let amt = b.build_int_const(33u64, ValueType::I64)?; // I64 — wider!
        let node = b.create_node(
            NodeKind::IntBinaryOp(IntBinaryOp::ShiftLeft),
            [lhs, amt],
            [ValueKind::Typed(ValueType::I32)],
        );
        let [out] = b
            .node_outputs_exact::<1>(node)
            .expect("binary op has 1 output");
        Ok(out)
    })?;
    let outcome = crate::pipeline::run_one(&ConstantFold::new(), &mut fg, &mut crate::OptCtx::new(None))?;
    assert!(
        !outcome.changed(),
        "width-mismatched binary fold must skip, not fold"
    );
    assert!(
        matches!(
            return_kind(fg.graph())?,
            NodeKind::IntBinaryOp(IntBinaryOp::ShiftLeft)
        ),
        "the ShiftLeft node must remain (fold skipped)"
    );
    Ok(())
}

/// Width-consistency guard (M5): `eval_int_cmp` masks both operands to the
/// *LHS* width.  A `Sless` with a wider RHS would have its RHS masked down,
/// silently changing the compared value and possibly flipping the verdict.
/// The guard must skip the fold on a width mismatch.
#[test]
fn int_cmp_fold_skips_width_mismatched_operands() -> Result<()> {
    use strider_ir::IRBuilder;
    use strider_ir::node::ValueKind;
    // Sless(IntConst:I8(0x80), IntConst:I32(200)) — LHS I8, RHS I32.
    let mut fg = make_fn(|b| {
        let lhs = b.build_int_const(0x80u64, ValueType::I8)?; // I8
        let rhs = b.build_int_const(200u64, ValueType::I32)?; // I32 — wider!
        let node = b.create_node(
            NodeKind::IntCmpOp(IntCmpOp::Sless),
            [lhs, rhs],
            [ValueKind::Typed(ValueType::I1)],
        );
        let [out] = b
            .node_outputs_exact::<1>(node)
            .expect("cmp op has 1 output");
        Ok(out)
    })?;
    let outcome = crate::pipeline::run_one(&ConstantFold::new(), &mut fg, &mut crate::OptCtx::new(None))?;
    assert!(
        !outcome.changed(),
        "width-mismatched cmp fold must skip, not fold"
    );
    assert!(
        matches!(
            return_kind(fg.graph())?,
            NodeKind::IntCmpOp(IntCmpOp::Sless)
        ),
        "the Sless node must remain (fold skipped)"
    );
    Ok(())
}

// ── integer binary folding ────────────────────────────────────────────────

/// Case table for the run-once two-const integer binary folds.  Each row
/// names the stand-alone test it absorbed: build
/// `op(IntConst(lhs), IntConst(rhs))` at `ty`, run `ConstantFold` once,
/// assert it fired, and assert the function now returns
/// `IntConst(expected)`.
#[test]
fn fold_int_binary_two_consts_cases() -> Result<()> {
    struct Case {
        case: &'static str,
        lhs: u128,
        rhs: u128,
        op: IntBinaryOp,
        ty: ValueType,
        expected: u64,
    }
    #[rustfmt::skip]
    let cases = [
        Case { case: "fold_int_add_consts", lhs: 3, rhs: 4, op: IntBinaryOp::Add, ty: ValueType::I64, expected: 7 },
        Case { case: "fold_int_and_zero", lhs: 0xFF, rhs: 0, op: IntBinaryOp::And, ty: ValueType::I64, expected: 0 },
        Case { case: "fold_mul_by_one", lhs: 5, rhs: 1, op: IntBinaryOp::Mul, ty: ValueType::I64, expected: 5 },
        // Shift constant evaluation: `1 << 4` for I32 -> 0x10.
        Case { case: "fold_shl_const_u32", lhs: 1, rhs: 4, op: IntBinaryOp::ShiftLeft, ty: ValueType::I32, expected: 0x10 },
        // Shift at width boundary: `1 << 31` for I32 -> 0x80000000.
        Case { case: "fold_shl_at_width_boundary_u32", lhs: 1, rhs: 31, op: IntBinaryOp::ShiftLeft, ty: ValueType::I32, expected: 0x8000_0000 },
        // Shift right: `0x80 >> 7` for I8 -> 1.
        Case { case: "fold_shr_const_u8", lhs: 0x80, rhs: 7, op: IntBinaryOp::ShiftRight, ty: ValueType::I8, expected: 1 },
        // `Xor(49, ~0)` at I32 must fold to bitwise NOT (= ~49 = 0xFFFF_FFCE)
        // -- NOT two's complement (-49).
        Case { case: "fold_int_unary_neg_is_bitwise_not_u32", lhs: 49, rhs: u128::MAX, op: IntBinaryOp::Xor, ty: ValueType::I32, expected: 0xFFFF_FFCE },
        // At I8: `Xor(0xAA, 0xFF)` must fold to `~0xAA = 0x55` (bitwise NOT).
        Case { case: "fold_int_unary_neg_intermediate_is_bitwise_not_u8", lhs: 0xAA, rhs: u128::MAX, op: IntBinaryOp::Xor, ty: ValueType::I8, expected: 0x55 },
        // Bitwise NOT of 0 is all-ones at the type width -- `Xor(0, ~0) = ~0`.
        Case { case: "fold_int_unary_neg_zero_is_all_ones_u32", lhs: 0, rhs: u128::MAX, op: IntBinaryOp::Xor, ty: ValueType::I32, expected: 0xFFFF_FFFF },
    ];
    for c in &cases {
        let mut fg = make_fn(|b| {
            let lhs = b.build_int_const(c.lhs, c.ty)?;
            let rhs = b.build_int_const(c.rhs, c.ty)?;
            b.build_int_binary_operation(lhs, rhs, c.op, c.ty)
        })?;
        assert!(
            crate::pipeline::run_one(&ConstantFold::new(), &mut fg, &mut crate::OptCtx::new(None))?
                .changed(),
            "{}: ConstantFold must fold the two-const {:?}",
            c.case,
            c.op,
        );
        {
            let ret_val = return_value(fg.graph())?;
            assert!(
                fg.int_const_u128(ret_val) == Some(u128::from(c.expected)),
                "{}: expected IntConst({:#x}), got {:?}",
                c.case,
                c.expected,
                fg.int_const_u128(ret_val),
            );
        }
    }
    Ok(())
}

#[test]
fn fold_int_xor_self() -> Result<()> {
    let mut fg = make_fn(|b| {
        let x = b.build_int_const(0xABu64, ValueType::I64).unwrap();
        b.build_int_binary_operation(x, x, IntBinaryOp::Xor, ValueType::I64)
    })?;
    assert!(
        crate::pipeline::run_one(&ConstantFold::new(), &mut fg, &mut crate::OptCtx::new(None))?
            .changed()
    );
    assert_returns_const(&fg, 0);
    Ok(())
}

#[test]
fn fold_int_sub_self() -> Result<()> {
    let mut fg = make_fn(|b| {
        let x = b.build_int_const(0xABu64, ValueType::I64).unwrap();
        b.build_sub_as_add_neg(x, x, ValueType::I64)
    })?;
    assert!(
        crate::pipeline::run_one(&ConstantFold::new(), &mut fg, &mut crate::OptCtx::new(None))?
            .changed()
    );
    assert_returns_const(&fg, 0);
    Ok(())
}

#[test]
fn fold_add_zero_identity() -> Result<()> {
    // x + 0 → x  (x is non-const)
    let mut fg = make_fn(|b| {
        let c1 = b.build_int_const(1u64, ValueType::I64).unwrap();
        let c2 = b.build_int_const(2u64, ValueType::I64).unwrap();
        let x = b.build_int_binary_operation(c1, c2, IntBinaryOp::Add, ValueType::I64)?;
        let zero = b.build_int_const(0u64, ValueType::I64).unwrap();
        b.build_int_binary_operation(x, zero, IntBinaryOp::Add, ValueType::I64)
    })?;
    // After at least one fold pass x+0 should collapse to x, then x folds too.
    run_to_fixed_point(&ConstantFold::new(), &mut fg)?;
    assert_returns_const(&fg, 3);
    Ok(())
}

/// `(x & 4) & 7`  — bit 2 is the only bit reachable by both masks, so the
/// merged constant is `4 & 7 = 4`.
#[test]
fn fold_and_and_masks() -> Result<()> {
    let mut fg = make_fn(|b| {
        let x = b.build_int_const(0xFFu64, ValueType::I64).unwrap();
        let c4 = b.build_int_const(4u64, ValueType::I64).unwrap();
        let c7 = b.build_int_const(7u64, ValueType::I64).unwrap();
        let inner = b.build_int_binary_operation(x, c4, IntBinaryOp::And, ValueType::I64)?;
        b.build_int_binary_operation(inner, c7, IntBinaryOp::And, ValueType::I64)
    })?;
    // Run to convergence (both-const fold + mask-merge may each fire once).
    run_to_fixed_point(&ConstantFold::new(), &mut fg)?;
    // 0xFF & 4 = 4, 4 & 7 = 4.
    assert_returns_const(&fg, 4);
    Ok(())
}

// ── add/sub reassociation with constants ──────────────────────────────────

/// Asserts the return-value node is `expected_base + expected_const`
/// (type-masked; operand order irrelevant).
fn assert_add_with_const(
    fg: &strider_ir::Function,
    expected_base: strider_ir::Value,
    expected_const: u64,
    ty: ValueType,
) -> Result<()> {
    let val = return_value(fg.graph())?;
    let node = fg.producer(val);
    assert!(
        matches!(fg.node_kind(node), NodeKind::IntBinaryOp(IntBinaryOp::Add)),
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
        matches!(fg.kind_of_value(o), NodeKind::IntConst(_))
            && ty.get_unsigned_int(fg.int_const_u128(o).unwrap_or(u128::MAX)) == Some(masked)
    };
    let ok = (l == expected_base && const_on(r)) || (r == expected_base && const_on(l));
    assert!(
        ok,
        "expected `base + {:#x}`; got lhs kind={:?}, rhs kind={:?}",
        masked,
        fg.kind_of_value(l),
        fg.kind_of_value(r),
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
    fg: &strider_ir::Function,
    expected_base: strider_ir::Value,
    expected_const: u64,
    ty: ValueType,
) -> Result<()> {
    let val = return_value(fg.graph())?;
    let node = fg.producer(val);
    assert!(
        matches!(fg.node_kind(node), NodeKind::IntBinaryOp(IntBinaryOp::Add)),
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
    let const_match = |value: strider_ir::Value| {
        matches!(fg.kind_of_value(value), NodeKind::IntConst(_))
            && ty.get_unsigned_int(fg.int_const_u128(value).unwrap_or(u128::MAX))
                == Some(neg_masked)
    };
    let ok = (l == expected_base && const_match(r)) || (r == expected_base && const_match(l));
    assert!(
        ok,
        "expected `base + {:#x}` (= base - {:#x} canonicalised); got lhs kind={:?}, rhs kind={:?}",
        neg_masked,
        expected_const,
        fg.kind_of_value(l),
        fg.kind_of_value(r),
    );
    Ok(())
}

/// Commutative const-on-right canonicalisation: `Add(C, x)` with the const on
/// the *left* is rewritten to `Add(x, C)` so a constant operand is always the
/// right one.  The variable `x` is a register read (genuinely non-const), so
/// the canonicalisation fires (and doesn't ping-pong like a `(C1, C2)` pair).
/// Regression: a count-/unary-fold rule binds `v: uint`
/// (`int_const_u128` → `get_unsigned_int`). An I256 `Wide` const does not
/// fit `u128`, so `get_uint` returns `None`. The fold must treat that as a
/// clean *skip* (rule doesn't fire, IR unchanged) — not a hard
/// "missing binding for capture of kind uint" error that aborts the pass.
#[test]
fn unary_fold_skips_wide_const_cleanly() -> Result<()> {
    // Neg(IntConst(I256)) — exercises rule 2 (`v: uint`).
    let mut fg = make_fn(|b| {
        // A 256-bit value with a non-zero high limb so it cannot fit u128.
        let c = b.build_int_const_limbs(&[1, 2, 3, 4], ValueType::I256)?;
        b.build_int_unary_operation(c, IntUnaryOp::Neg, ValueType::I256)
    })?;
    // Must not error (the bug surfaced here as `Err(missing binding …)`).
    let outcome = crate::pipeline::run_one(&ConstantFold::new(), &mut fg, &mut crate::OptCtx::new(None))?;
    // The wide const can't fold, so nothing should change.
    assert!(
        !outcome.changed(),
        "wide-const unary fold must skip, leaving the IR untouched"
    );
    let ret = return_value(fg.graph())?;
    assert!(
        matches!(
            fg.node_kind(fg.producer(ret)),
            NodeKind::IntUnaryOp(IntUnaryOp::Neg)
        ),
        "the Neg node must remain (fold skipped)"
    );
    Ok(())
}

#[test]
fn canonicalize_commutative_const_to_right() -> Result<()> {
    let vn = reg_vn(0x1000, 8);
    let (mut fg, _x) = make_fn_with_var(vn, |b, x| {
        let c = b.build_int_const(5u64, ValueType::I64).unwrap();
        // const on the LEFT: Add(5, x).
        b.build_int_binary_operation(c, x, IntBinaryOp::Add, ValueType::I64)
    })?;
    run_to_fixed_point(&ConstantFold::new(), &mut fg)?;
    let ret = return_value(fg.graph())?;
    let add = fg.producer(ret);
    assert!(
        matches!(fg.node_kind(add), NodeKind::IntBinaryOp(IntBinaryOp::Add)),
        "result must still be an Add"
    );
    let inputs = fg.node_inputs(add);
    assert!(
        !matches!(fg.node_kind(fg.producer(inputs[0])), NodeKind::IntConst(_)),
        "operand 0 must be the variable, not the const"
    );
    assert!(
        matches!(fg.node_kind(fg.producer(inputs[1])), NodeKind::IntConst(_))
            && fg.int_const_u128(inputs[1]) == Some(5),
        "operand 1 must be the const (canonicalised to the right)"
    );
    Ok(())
}

#[test]
fn reassoc_add_add_consts() -> Result<()> {
    // (x + 3) + 4 → x + 7
    let vn = reg_vn(0x1000, 8);
    let (mut fg, x) = make_fn_with_var(vn, |b, x| {
        let c3 = b.build_int_const(3u64, ValueType::I64).unwrap();
        let c4 = b.build_int_const(4u64, ValueType::I64).unwrap();
        let inner = b.build_int_binary_operation(x, c3, IntBinaryOp::Add, ValueType::I64)?;
        b.build_int_binary_operation(inner, c4, IntBinaryOp::Add, ValueType::I64)
    })?;
    run_to_fixed_point(&ConstantFold::new(), &mut fg)?;
    assert_add_with_const(&fg, x, 7, ValueType::I64)?;
    Ok(())
}

#[test]
fn reassoc_add_sub_consts() -> Result<()> {
    // (x - 3) + 4 → x + 1
    let vn = reg_vn(0x1000, 8);
    let (mut fg, x) = make_fn_with_var(vn, |b, x| {
        let c3 = b.build_int_const(3u64, ValueType::I64).unwrap();
        let c4 = b.build_int_const(4u64, ValueType::I64).unwrap();
        let inner = b.build_sub_as_add_neg(x, c3, ValueType::I64)?;
        b.build_int_binary_operation(inner, c4, IntBinaryOp::Add, ValueType::I64)
    })?;
    run_to_fixed_point(&ConstantFold::new(), &mut fg)?;
    assert_add_with_const(&fg, x, 1, ValueType::I64)?;
    Ok(())
}

#[test]
fn reassoc_sub_add_consts_wrapping() -> Result<()> {
    // (x + 3) - 4 → x + (3 - 4)  = x + 0xFFFF_FFFF_FFFF_FFFF
    let vn = reg_vn(0x1000, 8);
    let (mut fg, x) = make_fn_with_var(vn, |b, x| {
        let c3 = b.build_int_const(3u64, ValueType::I64).unwrap();
        let c4 = b.build_int_const(4u64, ValueType::I64).unwrap();
        let inner = b.build_int_binary_operation(x, c3, IntBinaryOp::Add, ValueType::I64)?;
        b.build_sub_as_add_neg(inner, c4, ValueType::I64)
    })?;
    run_to_fixed_point(&ConstantFold::new(), &mut fg)?;
    assert_add_with_const(&fg, x, 0xFFFF_FFFF_FFFF_FFFF, ValueType::I64)?;
    Ok(())
}

#[test]
fn reassoc_sub_sub_consts() -> Result<()> {
    // (x - 3) - 4 → x - 7
    let vn = reg_vn(0x1000, 8);
    let (mut fg, x) = make_fn_with_var(vn, |b, x| {
        let c3 = b.build_int_const(3u64, ValueType::I64).unwrap();
        let c4 = b.build_int_const(4u64, ValueType::I64).unwrap();
        let inner = b.build_sub_as_add_neg(x, c3, ValueType::I64)?;
        b.build_sub_as_add_neg(inner, c4, ValueType::I64)
    })?;
    run_to_fixed_point(&ConstantFold::new(), &mut fg)?;
    assert_sub_with_const(&fg, x, 7, ValueType::I64)?;
    Ok(())
}

#[test]
fn reassoc_add_commuted_inner() -> Result<()> {
    // (3 + x) + 4 → x + 7 (inner Add has const on lhs)
    let vn = reg_vn(0x1000, 8);
    let (mut fg, x) = make_fn_with_var(vn, |b, x| {
        let c3 = b.build_int_const(3u64, ValueType::I64).unwrap();
        let c4 = b.build_int_const(4u64, ValueType::I64).unwrap();
        let inner = b.build_int_binary_operation(c3, x, IntBinaryOp::Add, ValueType::I64)?;
        b.build_int_binary_operation(inner, c4, IntBinaryOp::Add, ValueType::I64)
    })?;
    run_to_fixed_point(&ConstantFold::new(), &mut fg)?;
    assert_add_with_const(&fg, x, 7, ValueType::I64)?;
    Ok(())
}

#[test]
fn reassoc_add_commuted_outer() -> Result<()> {
    // 4 + (x + 3) → x + 7 (outer Add has const on lhs)
    let vn = reg_vn(0x1000, 8);
    let (mut fg, x) = make_fn_with_var(vn, |b, x| {
        let c3 = b.build_int_const(3u64, ValueType::I64).unwrap();
        let c4 = b.build_int_const(4u64, ValueType::I64).unwrap();
        let inner = b.build_int_binary_operation(x, c3, IntBinaryOp::Add, ValueType::I64)?;
        b.build_int_binary_operation(c4, inner, IntBinaryOp::Add, ValueType::I64)
    })?;
    run_to_fixed_point(&ConstantFold::new(), &mut fg)?;
    assert_add_with_const(&fg, x, 7, ValueType::I64)?;
    Ok(())
}

#[test]
fn reassoc_chain_three_subs() -> Result<()> {
    // ((x - 4) - 4) - 4 → x - 12.  Requires the fixed-point loop to
    // compose multiple reassociation steps.
    let vn = reg_vn(0x1000, 8);
    let (mut fg, x) = make_fn_with_var(vn, |b, x| {
        let c4 = b.build_int_const(4u64, ValueType::I64).unwrap();
        let a = b.build_sub_as_add_neg(x, c4, ValueType::I64)?;
        let b_ = b.build_sub_as_add_neg(a, c4, ValueType::I64)?;
        b.build_sub_as_add_neg(b_, c4, ValueType::I64)
    })?;
    run_to_fixed_point(&ConstantFold::new(), &mut fg)?;
    assert_sub_with_const(&fg, x, 12, ValueType::I64)?;
    Ok(())
}

#[test]
fn reassoc_chain_three_subs_u32() -> Result<()> {
    // Same chain but at I32: ((x - 4) - 4) - 4 → x - 12.
    let vn = reg_vn(0x1000, 4);
    let (mut fg, x) = make_fn_with_var(vn, |b, x| {
        let c4 = b.build_int_const(4u64, ValueType::I32).unwrap();
        let a = b.build_sub_as_add_neg(x, c4, ValueType::I32)?;
        let b_ = b.build_sub_as_add_neg(a, c4, ValueType::I32)?;
        b.build_sub_as_add_neg(b_, c4, ValueType::I32)
    })?;
    run_to_fixed_point(&ConstantFold::new(), &mut fg)?;
    assert_sub_with_const(&fg, x, 12, ValueType::I32)?;
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
    let inner = b.build_int_binary_operation(x, y, IntBinaryOp::Add, ValueType::I64)?;
    let outer = b.build_int_binary_operation(inner, z, IntBinaryOp::Add, ValueType::I64)?;
    b.build_return(Some(outer), &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;
    let before = return_value(fg.graph())?;
    // Should not change: no constants anywhere.
    let res = crate::pipeline::run_one(&ConstantFold::new(), &mut fg, &mut crate::OptCtx::new(None))?;
    assert!(!res.changed(), "no-const chain should not reassociate");
    assert_eq!(return_value(fg.graph())?, before);
    Ok(())
}

/// Builds `((a & C1) | (b & C2)) & C3` (the AND-distribution LHS) and
/// returns the built function plus the outer-And's count of `Or` nodes.
fn build_and_dist_fn(c1: u64, c2: u64, c3: u64) -> Result<strider_ir::Function> {
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
    let k1 = b.build_int_const(c1, ValueType::I64).unwrap();
    let k2 = b.build_int_const(c2, ValueType::I64).unwrap();
    let k3 = b.build_int_const(c3, ValueType::I64).unwrap();
    let a_and = b.build_int_binary_operation(a, k1, IntBinaryOp::And, ValueType::I64)?;
    let b_and = b.build_int_binary_operation(bval, k2, IntBinaryOp::And, ValueType::I64)?;
    let or_node = b.build_int_binary_operation(a_and, b_and, IntBinaryOp::Or, ValueType::I64)?;
    let outer = b.build_int_binary_operation(or_node, k3, IntBinaryOp::And, ValueType::I64)?;
    b.build_return(Some(outer), &[])?;
    b.set_lift_addr(None);
    b.build()
}

/// Counts the *reachable* `Or` nodes (walks from entry, ignoring detached
/// zombies left behind by rewrites).
fn reachable_or_nodes(fg: &strider_ir::Function) -> usize {
    fg.count_kind(|k| matches!(k, NodeKind::IntBinaryOp(IntBinaryOp::Or)))
}

/// The register-merge-mask case the AND-distribution rule exists for: one
/// product `Ci & C3` is zero, so a disjunct collapses and the `Or`
/// disappears.  C1 & C3 = 0xFFFF_0000 & 0x0000_FFFF = 0 → the `(a & C1)`
/// disjunct folds to `(a & 0) → 0`, and `Or(0, b & C2) → b & C2`, leaving a
/// single `And` as the returned value.
#[test]
fn distribution_rewrite_simplifies_when_a_product_is_zero() -> Result<()> {
    let mut fg = build_and_dist_fn(0xFFFF_0000, 0x0000_FFFF, 0x0000_FFFF)?;
    assert_eq!(reachable_or_nodes(&fg), 1, "test setup expects one Or node");
    let changed = crate::pipeline::run_one(&ConstantFold::new(), &mut fg, &mut crate::OptCtx::new(None))?
        .changed();
    assert!(changed, "distribution rule should fire and simplify");
    // Drive ConstantFold to a true fixed point (each `run_one` only
    // re-seeds the directly-rewritten node, so the cascade
    // `(a & 0) → 0` → `Or(0, …) → …` settles over a couple of passes).
    for _ in 0..8 {
        if !crate::pipeline::run_one(&ConstantFold::new(), &mut fg, &mut crate::OptCtx::new(None))?
            .changed()
        {
            break;
        }
    }
    assert_eq!(
        reachable_or_nodes(&fg),
        0,
        "the disjunct should collapse and the Or disappear",
    );
    // The surviving return value is a single masking And, not an Or.
    assert!(
        matches!(
            return_kind(fg.graph())?,
            NodeKind::IntBinaryOp(IntBinaryOp::And)
        ),
        "expected the returned value to be a single masking And",
    );
    Ok(())
}

/// The ARM/Thumb dispatch alignment idiom: `(x | A) & B` where the OR-set
/// bits `A` are entirely cleared by the mask `B` (`A & B == 0`) is equal to
/// `x & B` — every surviving bit (`B_i = 1`) has `A_i = 0`, so
/// `(x_i | A_i) & B_i = x_i & B_i`.  `(x | 1) & 0xFFFF_FFFE` (set then clear
/// the Thumb bit) must fold to `x & 0xFFFF_FFFE`, dropping the `Or` so the
/// jump-table classifier sees a single masked load.
#[test]
fn align_or_removal_drops_or_when_set_bits_are_masked_off() -> Result<()> {
    let xv = reg_vn(0x1000, 8);
    let mut b = RegisterSet::new()
        .tracked(xv)
        .arg(xv)
        .build_fn_single_region()?;
    let x = b.read_variable(&xv)?;
    let one = b.build_int_const(1u64, ValueType::I64)?;
    let mask = b.build_int_const(0xFFFF_FFFEu64, ValueType::I64)?;
    let or_node = b.build_int_binary_operation(x, one, IntBinaryOp::Or, ValueType::I64)?;
    let outer = b.build_int_binary_operation(or_node, mask, IntBinaryOp::And, ValueType::I64)?;
    b.build_return(Some(outer), &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;
    assert_eq!(reachable_or_nodes(&fg), 1, "test setup expects one Or node");

    let changed = crate::pipeline::run_one(&ConstantFold::new(), &mut fg, &mut crate::OptCtx::new(None))?
        .changed();
    assert!(changed, "alignment-OR-removal rule should fire");
    for _ in 0..8 {
        if !crate::pipeline::run_one(&ConstantFold::new(), &mut fg, &mut crate::OptCtx::new(None))?
            .changed()
        {
            break;
        }
    }
    assert_eq!(
        reachable_or_nodes(&fg),
        0,
        "the masked-off Or must disappear, leaving a single masking And",
    );
    assert!(
        matches!(
            return_kind(fg.graph())?,
            NodeKind::IntBinaryOp(IntBinaryOp::And)
        ),
        "expected the returned value to be a single masking And",
    );
    Ok(())
}

/// Confluence regression: when BOTH products `C1 & C3` and `C2 & C3` are
/// non-zero, the AND-distribution is pure churn (it just pushes `& C3`
/// inward; neither disjunct can collapse), so the gated rule must NOT
/// fire.  Before the `when_match` guard this term re-distributed forever,
/// and the pass only terminated thanks to `compute_full`'s canonical RPO
/// settling operands first.  The guard makes the rule strictly
/// progress-reducing, so a second run reports `NoChange` (a stable fixed
/// point) and the factored `Or` shape is preserved.
#[test]
fn distribution_does_not_churn_when_both_products_nonzero() -> Result<()> {
    // C1 = C2 = C3 = 0xFF → both products = 0xFF (non-zero).
    let mut fg = build_and_dist_fn(0xFF, 0xFF, 0xFF)?;
    assert_eq!(reachable_or_nodes(&fg), 1, "test setup expects one Or node");

    // First run reaches the pass's internal fixed point without hanging.
    crate::pipeline::run_one(&ConstantFold::new(), &mut fg, &mut crate::OptCtx::new(None))?;

    // The factored shape is preserved — the Or is still there (the rule
    // did not distribute it away into churn).
    assert_eq!(
        reachable_or_nodes(&fg),
        1,
        "the factored Or shape must be preserved (no churn)",
    );

    // Re-running converges immediately: the result is a stable fixed point.
    let second = crate::pipeline::run_one(&ConstantFold::new(), &mut fg, &mut crate::OptCtx::new(None))?
        .changed();
    assert!(
        !second,
        "result must be a stable fixed point (no further change)"
    );
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
        let wide = b.build_int_const(0xFF00u64, ValueType::I16).unwrap();
        b.truncate_if_needed(wide, ValueType::I8)
    })?;
    let val = return_value(fg.graph())?;
    // Use int_const_u128 which masks to the declared type.
    let semantic = fg.int_const_u128(val);
    assert_eq!(semantic, Some(0), "0xFF00 truncated to I8 should be 0");
    // No Truncate nodes should exist.
    assert!(
        !fg.graph()
            .all_node_ids()
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
        let a = b.build_int_const(0xFFFFu64, ValueType::I16).unwrap();
        let b_ = b.build_int_const(0xFFFFu64, ValueType::I16).unwrap();
        // Non-const node so truncate_if_needed emits a real Truncate node.
        let or = b.build_int_binary_operation(a, b_, IntBinaryOp::Or, ValueType::I16)?;
        b.truncate_if_needed(or, ValueType::I8)
    })?;
    // Sanity: builder did emit a Truncate node.
    assert!(
        fg.graph()
            .all_node_ids()
            .any(|n| matches!(fg.node_kind(n), NodeKind::Truncate)),
        "test setup expects a Truncate node before optimization",
    );

    assert!(
        crate::pipeline::run_one(&ConstantFold::new(), &mut fg, &mut crate::OptCtx::new(None))?
            .changed()
    );

    // After optimization the Return's value must be an `IntConst(0xFF)`,
    // i.e. the low byte of 0xFFFF — *masked* to I8. A pre-fix run would
    // store `0xFFFF` (the wider raw value) here.
    let val = return_value(fg.graph())?;
    let raw = fg.int_const_u128(val).unwrap_or_else(|| {
        panic!(
            "expected IntConst producer for Return value, got {:?}",
            fg.kind_of_value(val)
        )
    });
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
        let a = b.build_int_const(0xAAu64, ValueType::I32).unwrap();
        let bb = b.build_int_const(0x55u64, ValueType::I32).unwrap();
        let or = b.build_int_binary_operation(a, bb, IntBinaryOp::Or, ValueType::I32)?;
        let widened = b.extend_if_needed(or, ValueType::I64, ExtendOp::ZeroExtend)?;
        b.truncate_if_needed(widened, ValueType::I32)
    })?;
    run_to_fixed_point(&ConstantFold::new(), &mut fg)?;
    // After optimization the Or's two const inputs fold to IntConst(0xFF),
    // and the Truncate(Extend(IntConst(0xFF))) collapses to IntConst(0xFF).
    // Most importantly: no Truncate or Extend node remains in the chain.
    let val = return_value(fg.graph())?;
    assert!(
        matches!(fg.kind_of_value(val), NodeKind::IntConst(_)),
        "round-trip + const-fold must leave an IntConst at the root, got {:?}",
        fg.kind_of_value(val)
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

/// `ZeroExtend(ZeroExtend(x)) → ZeroExtend(x)` — nested zero-extends compose
/// to a single zero-extend at the outer width (value preserved at every step).
#[test]
fn fold_nested_zero_extend_collapses() -> Result<()> {
    let xv = reg_vn(0x1000, 1); // I8 register
    let mut b = RegisterSet::new()
        .tracked(xv)
        .arg(xv)
        .build_fn_single_region()?;
    let x = b.read_variable(&xv)?;
    let w16 = b.extend_if_needed(x, ValueType::I16, ExtendOp::ZeroExtend)?;
    let w32 = b.extend_if_needed(w16, ValueType::I32, ExtendOp::ZeroExtend)?;
    b.build_return(Some(w32), &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;
    run_to_fixed_point(&ConstantFold::new(), &mut fg)?;
    let extends = fg
        .walk()
        .filter(|&n| matches!(fg.node_kind(n), NodeKind::Extend(ExtendOp::ZeroExtend)))
        .count();
    assert_eq!(
        extends, 1,
        "nested ZeroExtend must collapse to one ZeroExtend"
    );
    Ok(())
}

/// `SignExtend(SignExtend(x)) → SignExtend(x)` — sign replication is transitive.
#[test]
fn fold_nested_sign_extend_collapses() -> Result<()> {
    let xv = reg_vn(0x1000, 1);
    let mut b = RegisterSet::new()
        .tracked(xv)
        .arg(xv)
        .build_fn_single_region()?;
    let x = b.read_variable(&xv)?;
    let w16 = b.extend_if_needed(x, ValueType::I16, ExtendOp::SignExtend)?;
    let w32 = b.extend_if_needed(w16, ValueType::I32, ExtendOp::SignExtend)?;
    b.build_return(Some(w32), &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;
    run_to_fixed_point(&ConstantFold::new(), &mut fg)?;
    let extends = fg
        .walk()
        .filter(|&n| matches!(fg.node_kind(n), NodeKind::Extend(ExtendOp::SignExtend)))
        .count();
    assert_eq!(
        extends, 1,
        "nested SignExtend must collapse to one SignExtend"
    );
    Ok(())
}

/// `Truncate(Truncate(x)) → Truncate(x)` — narrowing twice equals one narrow
/// to the final width.
#[test]
fn fold_nested_truncate_collapses() -> Result<()> {
    let xv = reg_vn(0x1000, 4); // I32 register
    let mut b = RegisterSet::new()
        .tracked(xv)
        .arg(xv)
        .build_fn_single_region()?;
    let x = b.read_variable(&xv)?;
    let t16 = b.truncate_if_needed(x, ValueType::I16)?;
    let t8 = b.truncate_if_needed(t16, ValueType::I8)?;
    b.build_return(Some(t8), &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;
    run_to_fixed_point(&ConstantFold::new(), &mut fg)?;
    let truncs = fg
        .walk()
        .filter(|&n| matches!(fg.node_kind(n), NodeKind::Truncate))
        .count();
    assert_eq!(truncs, 1, "nested Truncate must collapse to one Truncate");
    Ok(())
}

/// Same identity holds for `SignExtend`: the sign bits added by the
/// extend are cut off by the truncate.
#[test]
fn fold_truncate_of_sign_extend_round_trip() -> Result<()> {
    let mut fg = make_fn(|b| {
        // Use a non-const Or so the rule fires through the inner expression
        // rather than via direct constant folding.
        let a = b.build_int_const(0x80u64, ValueType::I32).unwrap();
        let bb = b.build_int_const(0x01u64, ValueType::I32).unwrap();
        let or = b.build_int_binary_operation(a, bb, IntBinaryOp::Or, ValueType::I32)?;
        let widened = b.extend_if_needed(or, ValueType::I64, ExtendOp::SignExtend)?;
        b.truncate_if_needed(widened, ValueType::I32)
    })?;
    run_to_fixed_point(&ConstantFold::new(), &mut fg)?;
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
        let lhs = b.build_int_const(3u64, ValueType::I32).unwrap();
        let rhs = b.build_int_const(7u64, ValueType::I32).unwrap();
        // Use non-const expressions so the constant folder doesn't
        // collapse before our rule runs.
        let lhs_or = b.build_int_binary_operation(lhs, lhs, IntBinaryOp::Or, ValueType::I32)?;
        let rhs_or = b.build_int_binary_operation(rhs, rhs, IntBinaryOp::Or, ValueType::I32)?;
        let lhs_ext = b.extend_if_needed(lhs_or, ValueType::I64, ExtendOp::SignExtend)?;
        let rhs_ext = b.extend_if_needed(rhs_or, ValueType::I64, ExtendOp::SignExtend)?;
        let mul =
            b.build_int_binary_operation(lhs_ext, rhs_ext, IntBinaryOp::Mul, ValueType::I64)?;
        b.truncate_if_needed(mul, ValueType::I32)
    })?;
    run_to_fixed_point(&ConstantFold::new(), &mut fg)?;
    // After narrowing-through-Mul + constant fold: 3 * 7 = 21 at I32.
    assert_returns_const(&fg, 21);
    // Nothing wider than I32 should survive (no SignExtend/Mul@I64/Truncate).
    for nid in fg.walk() {
        let kind = fg.node_kind(nid);
        assert!(
            !matches!(
                kind,
                NodeKind::Extend(ExtendOp::SignExtend) | NodeKind::Truncate
            ),
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
        let low_part = b.build_int_const(0xAAu64, ValueType::I64).unwrap();
        let junk = b
            .build_int_const(0x12345678_DEADBEEFu64, ValueType::I64)
            .unwrap();
        // Make low_part non-const via Or so the rule fires through it.
        let low_or =
            b.build_int_binary_operation(low_part, low_part, IntBinaryOp::Or, ValueType::I64)?;
        // High mask = 0xFFFF_FFFF_0000_0000 (low 32 bits are zero).
        let high_mask = b
            .build_int_const(0xFFFFFFFF_00000000u64, ValueType::I64)
            .unwrap();
        let high_part =
            b.build_int_binary_operation(high_mask, junk, IntBinaryOp::And, ValueType::I64)?;
        let merged =
            b.build_int_binary_operation(low_or, high_part, IntBinaryOp::Or, ValueType::I64)?;
        b.truncate_if_needed(merged, ValueType::I32)
    })?;
    run_to_fixed_point(&ConstantFold::new(), &mut fg)?;
    // After dropping the high half + folding 0xAA | 0xAA = 0xAA at I32:
    // the result is IntConst(0xAA).  No Or remains.
    assert_returns_const(&fg, 0xAA);
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
        let a = b
            .build_int_const(0x1234_5678_DEAD_BEEFu64, ValueType::I64)
            .unwrap();
        let x = b.build_int_binary_operation(a, a, IntBinaryOp::Or, ValueType::I64)?;
        let low_mask = b.build_int_const(0xFFFFFFFFu64, ValueType::I64).unwrap();
        let masked = b.build_int_binary_operation(low_mask, x, IntBinaryOp::And, ValueType::I64)?;
        b.truncate_if_needed(masked, ValueType::I32)
    })?;
    run_to_fixed_point(&ConstantFold::new(), &mut fg)?;
    // After dropping the redundant And + folding the OR-of-itself:
    // result is IntConst(0xDEADBEEF) at I32.
    assert_returns_const(&fg, 0xDEADBEEF);
    Ok(())
}

/// Covers the const-on-right And orientation of `drop_low_mask_under_truncate`
/// (`Truncate(And(x, low_mask))`). A single rule orientation handles it: the
/// non-const operand `x` fails `any_int_const` structurally, so the `And`'s own
/// commutative retry binds the const regardless of side.
#[test]
fn fold_drop_low_mask_under_truncate_const_on_right() -> Result<()> {
    let mut fg = make_fn(|b| {
        let a = b
            .build_int_const(0x1234_5678_DEAD_BEEFu64, ValueType::I64)
            .unwrap();
        let x = b.build_int_binary_operation(a, a, IntBinaryOp::Or, ValueType::I64)?;
        let low_mask = b.build_int_const(0xFFFFFFFFu64, ValueType::I64).unwrap();
        // Const on the RIGHT: And(x, low_mask).
        let masked = b.build_int_binary_operation(x, low_mask, IntBinaryOp::And, ValueType::I64)?;
        b.truncate_if_needed(masked, ValueType::I32)
    })?;
    run_to_fixed_point(&ConstantFold::new(), &mut fg)?;
    assert_returns_const(&fg, 0xDEADBEEF);
    Ok(())
}

/// Covers the And-term-on-left Or orientation of `drop_high_half_in_or_truncate`
/// (`Or(And(high_mask, junk), low_part)`). A single rule orientation handles it:
/// `low_part` fails the `and(...)` subpattern structurally, so the `Or`'s
/// commutative retry binds it regardless of side. (The two-`And` form
/// `Or(And, And)`, disambiguated only by the value guard on the unary truncate
/// ancestor, is exercised by `test_narrow_widths::x64` and relies on the
/// matcher's continuation-passing guard re-drive.)
#[test]
fn fold_drop_high_half_in_or_truncate_and_term_on_left() -> Result<()> {
    let mut fg = make_fn(|b| {
        let low_part = b.build_int_const(0xAAu64, ValueType::I64).unwrap();
        let junk = b
            .build_int_const(0x12345678_DEADBEEFu64, ValueType::I64)
            .unwrap();
        let low_or =
            b.build_int_binary_operation(low_part, low_part, IntBinaryOp::Or, ValueType::I64)?;
        let high_mask = b
            .build_int_const(0xFFFFFFFF_00000000u64, ValueType::I64)
            .unwrap();
        let high_part =
            b.build_int_binary_operation(high_mask, junk, IntBinaryOp::And, ValueType::I64)?;
        // And-term on the LEFT of the Or: Or(And(...), low_or).
        let merged =
            b.build_int_binary_operation(high_part, low_or, IntBinaryOp::Or, ValueType::I64)?;
        b.truncate_if_needed(merged, ValueType::I32)
    })?;
    run_to_fixed_point(&ConstantFold::new(), &mut fg)?;
    assert_returns_const(&fg, 0xAA);
    for nid in fg.walk() {
        assert!(
            !matches!(fg.node_kind(nid), NodeKind::IntBinaryOp(IntBinaryOp::Or)),
            "high-mask half drop must collapse the Or (And-term on left)"
        );
    }
    Ok(())
}

/// The round-trip rule must NOT fire when `x`'s type is *narrower* than
/// the truncate's output type — that's a real width-narrowing operation,
/// not an identity.  `Truncate_U16(Extend_U64(x_U32))` is still a real
/// truncation from I32 to I16.
#[test]
fn fold_truncate_of_extend_skips_when_widths_differ() -> Result<()> {
    let mut fg = make_fn(|b| {
        let a = b.build_int_const(0xAAu64, ValueType::I32).unwrap();
        let bb = b.build_int_const(0x55u64, ValueType::I32).unwrap();
        let or = b.build_int_binary_operation(a, bb, IntBinaryOp::Or, ValueType::I32)?;
        let widened = b.extend_if_needed(or, ValueType::I64, ExtendOp::ZeroExtend)?;
        // Truncate to I16 — narrower than the inner Or's I32 width, so the
        // round-trip rule must NOT fire.  Constant-fold can still collapse
        // the const-Or, but the truncate must remain (or its result must
        // still semantically be I16).
        b.truncate_if_needed(widened, ValueType::I16)
    })?;
    run_to_fixed_point(&ConstantFold::new(), &mut fg)?;
    // The result must be I16-typed.
    let val = return_value(fg.graph())?;
    assert_eq!(
        fg.value_kind(val),
        strider_ir::node::ValueKind::Typed(ValueType::I16),
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
        let one = b.build_int_const(u128::MAX, ValueType::I1)?;
        b.build_int_binary_operation(t, one, IntBinaryOp::Xor, ValueType::I1)
    })?;
    assert!(
        crate::pipeline::run_one(&ConstantFold::new(), &mut fg, &mut crate::OptCtx::new(None))?
            .changed()
    );
    assert_returns_const(&fg, 0);
    Ok(())
}

#[test]
fn fold_bool_and_consts() -> Result<()> {
    // `true & false` folds to `IntConst(0):I1` via the integer binary
    // const-fold rule.
    let mut fg = make_fn(|b| {
        let t = b.build_boolean_const(true);
        let f = b.build_boolean_const(false);
        b.build_int_binary_operation(t, f, IntBinaryOp::And, ValueType::I1)
    })?;
    assert!(
        crate::pipeline::run_one(&ConstantFold::new(), &mut fg, &mut crate::OptCtx::new(None))?
            .changed()
    );
    assert_returns_const(&fg, 0);
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
        let c5 = b.build_int_const(5u64, ValueType::I64).unwrap();
        // Non-const Bool: `x == 5`.
        let cmp = b.build_int_cmp_operation(x, c5, IntCmpOp::Equal, ValueType::I64)?;
        let t = b.build_boolean_const(true);
        b.build_int_binary_operation(cmp, t, IntBinaryOp::Xor, ValueType::I1)
    })?;
    // The const-fold pipeline may or may not "change" — the Xor shape
    // is already canonical for logical-NOT.  Assert the final shape.
    let _ = crate::pipeline::run_one(&ConstantFold::new(), &mut fg, &mut crate::OptCtx::new(None))?;
    assert_return_kind(fg.graph(), NodeKind::IntBinaryOp(IntBinaryOp::Xor));
    Ok(())
}

// `xor` is commutative, so `true ^ b` must rewrite the same as `b ^ true`.
#[test]
fn fold_bool_true_xor_x_to_not_commutative() -> Result<()> {
    let vn = reg_vn(0x1000, 8);
    let (mut fg, _x) = make_fn_with_var(vn, |b, x| {
        let c5 = b.build_int_const(5u64, ValueType::I64).unwrap();
        let cmp = b.build_int_cmp_operation(x, c5, IntCmpOp::Equal, ValueType::I64)?;
        let t = b.build_boolean_const(true);
        // Operands flipped relative to the previous test.
        b.build_int_binary_operation(t, cmp, IntBinaryOp::Xor, ValueType::I1)
    })?;
    let _ = crate::pipeline::run_one(&ConstantFold::new(), &mut fg, &mut crate::OptCtx::new(None))?;
    assert_return_kind(fg.graph(), NodeKind::IntBinaryOp(IntBinaryOp::Xor));
    Ok(())
}

// `xor b false` does not match the `x ^ all_ones → ~x` canonicalization
// (the const is `0`, not all-ones).  Instead the integer identity rule
// `x ^ 0 → x` fires, collapsing it to the cmp directly.
#[test]
fn no_fold_bool_xor_false() -> Result<()> {
    let vn = reg_vn(0x1000, 8);
    let (mut fg, _x) = make_fn_with_var(vn, |b, x| {
        let c5 = b.build_int_const(5u64, ValueType::I64).unwrap();
        let cmp = b.build_int_cmp_operation(x, c5, IntCmpOp::Equal, ValueType::I64)?;
        let f = b.build_boolean_const(false);
        b.build_int_binary_operation(cmp, f, IntBinaryOp::Xor, ValueType::I1)
    })?;
    assert!(
        crate::pipeline::run_one(&ConstantFold::new(), &mut fg, &mut crate::OptCtx::new(None))?
            .changed()
    );
    // `x ^ 0 → x`: the Xor collapses to the cmp, not to a BitNot.
    assert_return_kind(fg.graph(), NodeKind::IntCmpOp(IntCmpOp::Equal));
    Ok(())
}

// `x | true → true` for non-const x (Or-absorbing at I1; `true` is the
// all-ones value).  Pins the re-expressed `BOr(true, _) → true` rule —
// `Or` is commutative so `true | x` rewrites the same.
#[test]
fn fold_bool_or_true_to_true() -> Result<()> {
    let vn = reg_vn(0x1000, 8);
    let (mut fg, _x) = make_fn_with_var(vn, |b, x| {
        let c5 = b.build_int_const(5u64, ValueType::I64).unwrap();
        // Non-const Bool: `x == 5`.
        let cmp = b.build_int_cmp_operation(x, c5, IntCmpOp::Equal, ValueType::I64)?;
        let t = b.build_boolean_const(true);
        b.build_int_binary_operation(cmp, t, IntBinaryOp::Or, ValueType::I1)
    })?;
    assert!(
        crate::pipeline::run_one(&ConstantFold::new(), &mut fg, &mut crate::OptCtx::new(None))?
            .changed()
    );
    // `x | true → true`: folds to the constant 1 (true at I1), not the cmp.
    assert_returns_const(&fg, 1);
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
        let x32 = b.build_int_unary_operation(x, IntUnaryOp::Neg, ValueType::I32)?;
        let all_ones = b.build_int_const(0xFFFF_FFFFu64, ValueType::I32).unwrap();
        b.build_int_binary_operation(x32, all_ones, IntBinaryOp::Or, ValueType::I32)
    })?;
    assert!(
        crate::pipeline::run_one(&ConstantFold::new(), &mut fg, &mut crate::OptCtx::new(None))?
            .changed()
    );
    // Folds to the all-ones constant.
    assert_returns_const(&fg, 0xFFFF_FFFF);
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
        let c5 = b.build_int_const(5u64, ValueType::I64).unwrap();
        let cmp = b.build_int_cmp_operation(x, c5, IntCmpOp::Equal, ValueType::I64)?;
        let one = b.build_int_const(u128::MAX, ValueType::I1)?;
        let n1 = b.build_int_binary_operation(cmp, one, IntBinaryOp::Xor, ValueType::I1)?;
        b.build_int_binary_operation(n1, one, IntBinaryOp::Xor, ValueType::I1)
    })?;
    assert!(
        crate::pipeline::run_one(&ConstantFold::new(), &mut fg, &mut crate::OptCtx::new(None))?
            .changed()
    );
    // After fold the function returns the cmp directly.
    assert_return_kind(fg.graph(), NodeKind::IntCmpOp(IntCmpOp::Equal));
    Ok(())
}

// Composes with the xor-true rule via the fixed-point loop:
// `xor (xor b true) true` → `not (not b)` → `b`.
#[test]
fn fold_bool_xor_true_xor_true_collapses_to_x() -> Result<()> {
    let vn = reg_vn(0x1000, 8);
    let (mut fg, _x) = make_fn_with_var(vn, |b, x| {
        let c5 = b.build_int_const(5u64, ValueType::I64).unwrap();
        let cmp = b.build_int_cmp_operation(x, c5, IntCmpOp::Equal, ValueType::I64)?;
        let t1 = b.build_boolean_const(true);
        let xor1 = b.build_int_binary_operation(cmp, t1, IntBinaryOp::Xor, ValueType::I1)?;
        let t2 = b.build_boolean_const(true);
        b.build_int_binary_operation(xor1, t2, IntBinaryOp::Xor, ValueType::I1)
    })?;
    run_to_fixed_point(&ConstantFold::new(), &mut fg)?;
    assert_return_kind(fg.graph(), NodeKind::IntCmpOp(IntCmpOp::Equal));
    Ok(())
}

// ── no-fold edge cases ────────────────────────────────────────────────────

#[test]
fn no_fold_div_by_zero() -> Result<()> {
    let mut fg = make_fn(|b| {
        let x = b.build_int_const(10u64, ValueType::I64).unwrap();
        let zero = b.build_int_const(0u64, ValueType::I64).unwrap();
        b.build_int_binary_operation(x, zero, IntBinaryOp::Div, ValueType::I64)
    })?;
    // Should not fold (division by zero is undefined).
    assert!(
        !crate::pipeline::run_one(&ConstantFold::new(), &mut fg, &mut crate::OptCtx::new(None))?
            .changed()
    );
    assert!(matches!(
        return_kind(fg.graph())?,
        NodeKind::IntBinaryOp(IntBinaryOp::Div)
    ));
    Ok(())
}

/// Division / remainder by a constant 0 must skip on every divide-family
/// op at I32 (Sleigh leaves division-by-zero undefined, mirroring the I64
/// `no_fold_div_by_zero` pin above): the pass reports no change and the
/// op node survives as the return-value producer.
#[test]
fn no_fold_divide_family_by_zero_i32_cases() -> Result<()> {
    for op in [
        IntBinaryOp::Div,
        IntBinaryOp::Sdiv,
        IntBinaryOp::Rem,
        IntBinaryOp::Srem,
    ] {
        let mut fg = make_fn(|b| {
            let x = b.build_int_const(10u64, ValueType::I32).unwrap();
            let zero = b.build_int_const(0u64, ValueType::I32).unwrap();
            b.build_int_binary_operation(x, zero, op, ValueType::I32)
        })?;
        assert!(
            !crate::pipeline::run_one(&ConstantFold::new(), &mut fg, &mut crate::OptCtx::new(None))?
                .changed(),
            "{op:?} by const 0 must not fold (undefined)",
        );
        assert_eq!(
            return_kind(fg.graph())?,
            NodeKind::IntBinaryOp(op),
            "{op:?} node must survive the by-zero skip",
        );
    }
    Ok(())
}

/// Shifts by exactly `bit_width - 1` (the last in-range amount) fold to
/// the hand-computed value — one row per shift flavour at I32, including
/// the signed-fill SShiftRight on both a negative and a non-negative lhs.
#[test]
fn fold_shift_by_width_minus_one_cases() -> Result<()> {
    #[rustfmt::skip]
    let cases = [
        // 1 << 31 = 0x8000_0000 (the existing boundary row, kept for symmetry).
        ("shl_by_31", IntBinaryOp::ShiftLeft, 1u128, 0x8000_0000u128),
        // 0xFFFF_FFFF >> 31 = 1.
        ("lshr_by_31", IntBinaryOp::ShiftRight, 0xFFFF_FFFF, 1),
        // 0x8000_0000 >>s 31 = all-ones (sign fill).
        ("ashr_by_31_negative", IntBinaryOp::SShiftRight, 0x8000_0000, 0xFFFF_FFFF),
        // 0x7FFF_FFFF >>s 31 = 0 (sign bit clear).
        ("ashr_by_31_positive", IntBinaryOp::SShiftRight, 0x7FFF_FFFF, 0),
    ];
    for (case, op, lhs, expected) in cases {
        let mut fg = make_fn(|b| {
            let l = b.build_int_const(lhs, ValueType::I32)?;
            let s = b.build_int_const(31u64, ValueType::I32)?;
            b.build_int_binary_operation(l, s, op, ValueType::I32)
        })?;
        assert!(
            crate::pipeline::run_one(&ConstantFold::new(), &mut fg, &mut crate::OptCtx::new(None))?
                .changed(),
            "{case}: shift by width-1 must fold",
        );
        {
            let ret_val = return_value(fg.graph())?;
            assert!(
                fg.int_const_u128(ret_val) == Some(expected),
                "{case}: expected IntConst({expected:#x}), got {:?}",
                fg.int_const_u128(ret_val),
            );
        }
    }
    Ok(())
}

/// Arithmetic at the 1-bit width: `Add(1, 1):I1` wraps to 0 (the result
/// is masked to the declared width), `And(1, 1):I1` is 1, and
/// `Xor(1, 1):I1` is 0.
#[test]
fn fold_i1_arithmetic_cases() -> Result<()> {
    #[rustfmt::skip]
    let cases = [
        ("add_1_1_wraps_to_0", IntBinaryOp::Add, 0u128),
        ("and_1_1_is_1", IntBinaryOp::And, 1),
        ("xor_1_1_is_0", IntBinaryOp::Xor, 0),
    ];
    for (case, op, expected) in cases {
        let mut fg = make_fn(|b| {
            let a = b.build_int_const(1u64, ValueType::I1)?;
            let b_ = b.build_int_const(1u64, ValueType::I1)?;
            b.build_int_binary_operation(a, b_, op, ValueType::I1)
        })?;
        // `op(c, c)` identities (`x ^ x → 0`) may fire instead of the
        // two-const eval; either way the run must report a change and
        // land on the masked constant.
        assert!(
            crate::pipeline::run_one(&ConstantFold::new(), &mut fg, &mut crate::OptCtx::new(None))?
                .changed(),
            "{case}: I1 two-const op must fold",
        );
        {
            let ret_val = return_value(fg.graph())?;
            assert!(
                fg.int_const_u128(ret_val) == Some(expected),
                "{case}: expected IntConst({expected:#x}), got {:?}",
                fg.int_const_u128(ret_val),
            );
        }
    }
    Ok(())
}

#[test]
fn fold_int_cmp_equal_consts() -> Result<()> {
    let mut fg = make_fn(|b| {
        let c5 = b.build_int_const(5u64, ValueType::I64).unwrap();
        let c5b = b.build_int_const(5u64, ValueType::I64).unwrap();
        b.build_int_cmp_operation(c5, c5b, IntCmpOp::Equal, ValueType::I64)
    })?;
    assert!(
        crate::pipeline::run_one(&ConstantFold::new(), &mut fg, &mut crate::OptCtx::new(None))?
            .changed()
    );
    assert_returns_const(&fg, 1);
    Ok(())
}

#[test]
fn fold_int_cmp_less_consts() -> Result<()> {
    let mut fg = make_fn(|b| {
        let c3 = b.build_int_const(3u64, ValueType::I64).unwrap();
        let c5 = b.build_int_const(5u64, ValueType::I64).unwrap();
        b.build_int_cmp_operation(c3, c5, IntCmpOp::Less, ValueType::I64)
    })?;
    assert!(
        crate::pipeline::run_one(&ConstantFold::new(), &mut fg, &mut crate::OptCtx::new(None))?
            .changed()
    );
    assert_returns_const(&fg, 1);
    Ok(())
}

// ── Popcount / Lzcount ────────────────────────────────────────────────────

/// Case table for the run-once `Popcount` / `Lzcount` constant folds.
/// Each row names the stand-alone test it absorbed.
#[test]
fn fold_count_op_const_cases() -> Result<()> {
    #[derive(Clone, Copy)]
    enum CountOp {
        Popcount,
        Lzcount,
    }
    struct Case {
        case: &'static str,
        op: CountOp,
        input: u64,
        ty: ValueType,
        expected: u64,
    }
    #[rustfmt::skip]
    let cases = [
        // popcount(0b10110101) = 5.
        Case { case: "fold_popcount_const", op: CountOp::Popcount, input: 0b1011_0101, ty: ValueType::I8, expected: 5 },
        Case { case: "fold_popcount_zero", op: CountOp::Popcount, input: 0, ty: ValueType::I64, expected: 0 },
        // lzcount(0x80u8) = 0 (MSB is set).
        Case { case: "fold_lzcount_msb_set", op: CountOp::Lzcount, input: 0x80, ty: ValueType::I8, expected: 0 },
        // lzcount(1u8) = 7 (only bit 0 set in an 8-bit value).
        Case { case: "fold_lzcount_one", op: CountOp::Lzcount, input: 1, ty: ValueType::I8, expected: 7 },
        // lzcount(0_U32) must fold to 32 (the type's bit width). The previous
        // formula `(masked << (64 - bits)).leading_zeros()` returned 64 when
        // masked was 0, ignoring the type's narrower width.
        Case { case: "fold_lzcount_zero_u32", op: CountOp::Lzcount, input: 0, ty: ValueType::I32, expected: 32 },
        Case { case: "fold_lzcount_zero_u8", op: CountOp::Lzcount, input: 0, ty: ValueType::I8, expected: 8 },
        // I64 happened to work on the unfixed code (64 - 64 = 0 shift), but
        // pin it with a regression row so the fix doesn't break it.
        Case { case: "fold_lzcount_zero_u64", op: CountOp::Lzcount, input: 0, ty: ValueType::I64, expected: 64 },
    ];
    for c in &cases {
        let mut fg = make_fn(|b| {
            let v = b.build_int_const(c.input, c.ty).unwrap();
            match c.op {
                CountOp::Popcount => b.build_popcount(v, c.ty),
                CountOp::Lzcount => b.build_lzcount(v, c.ty),
            }
        })?;
        assert!(
            crate::pipeline::run_one(&ConstantFold::new(), &mut fg, &mut crate::OptCtx::new(None))?
                .changed(),
            "{}: ConstantFold must fold",
            c.case,
        );
        {
            let ret_val = return_value(fg.graph())?;
            assert!(
                fg.int_const_u128(ret_val) == Some(u128::from(c.expected)),
                "{}: expected IntConst({:#x}), got {:?}",
                c.case,
                c.expected,
                fg.int_const_u128(ret_val),
            );
        }
    }
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
/// the lower-level `Graph` mutators (which bypass build_int_const's I256 reject)
/// and rewire the Return.
fn build_unary_with_wide_const_input(
    kind: NodeKind,
    wide_ty: ValueType,
    out_ty: ValueType,
) -> Result<strider_ir::Function> {
    use strider_ir::node::ValueKind;
    let mut fg = make_fn(|b| Ok(b.build_int_const(0u64, ValueType::I64).unwrap()))?;
    let placeholder = return_value(fg.graph())?;
    // Intern the constant value (0xFF) at the wide type so we have a valid
    // ConstId for the new IntConst node.
    let const_id = fg.intern_int_const(0xFF_u128, wide_ty);
    let wide_node = fg.graph_mut().create_node(
        NodeKind::IntConst(const_id),
        [],
        [ValueKind::Typed(wide_ty)],
    );
    // Stamp the asm fingerprint on the new node (required by the validator).
    fg.extend_asm_fingerprint(wide_node, &[strider_ir_test_utils::SENTINEL_LIFT_ADDR]);
    let wide_const = fg.node_outputs_exact::<1>(wide_node)?[0];
    let unary_node = fg
        .graph_mut()
        .create_node(kind, [wide_const], [ValueKind::Typed(out_ty)]);
    fg.extend_asm_fingerprint(unary_node, &[strider_ir_test_utils::SENTINEL_LIFT_ADDR]);
    let unary_value = fg.node_outputs_exact::<1>(unary_node)?[0];
    fg.graph_mut().replace_all_uses(placeholder, unary_value);
    Ok(fg)
}

/// `Lzcount` / `Popcount` on an interner-width (I128 / I256) `IntConst`
/// can't be computed in u64 (`get_unsigned_int(wide, _) == None`), so the
/// fold rule must silently skip (`Error::skip`) rather than propagate
/// `ExpectedIntegerType` and crash the whole optimizer pipeline.  Each row
/// names the stand-alone test it absorbed.
#[test]
fn fold_count_op_wide_const_input_skips_cleanly_cases() -> Result<()> {
    #[rustfmt::skip]
    let cases = [
        ("fold_lzcount_u128_input_skips_cleanly", NodeKind::Lzcount, ValueType::I128),
        ("fold_lzcount_u256_input_skips_cleanly", NodeKind::Lzcount, ValueType::I256),
        ("fold_popcount_u128_input_skips_cleanly", NodeKind::Popcount, ValueType::I128),
        ("fold_popcount_u256_input_skips_cleanly", NodeKind::Popcount, ValueType::I256),
    ];
    for (case, kind, wide_ty) in cases {
        let mut fg = build_unary_with_wide_const_input(kind, wide_ty, ValueType::I64)?;
        let result = crate::pipeline::run_one(&ConstantFold::new(), &mut fg, &mut crate::OptCtx::new(None));
        assert!(
            result.is_ok(),
            "{case}: ConstantFold must not error on {kind:?}({wide_ty:?} const), got {:?}",
            result.err(),
        );
    }
    Ok(())
}

// ── Float constant folding ────────────────────────────────────────────────

/// Case table for the run-once two-const float binary folds.  Each row
/// names the stand-alone test it absorbed: build
/// `op(FloatConst(lhs), FloatConst(rhs))` at `ty`, run `ConstantFold`
/// once, and expect a fold to `FloatConst(expected)`.
#[test]
fn fold_float_binary_two_consts_cases() -> Result<()> {
    struct Case {
        case: &'static str,
        lhs: u64,
        rhs: u64,
        op: FloatBinaryOp,
        ty: ValueType,
        expected: u64,
    }
    #[rustfmt::skip]
    let cases = [
        Case { case: "fold_f32_add_consts", lhs: 3.0f32.to_bits() as u64, rhs: 4.0f32.to_bits() as u64, op: FloatBinaryOp::Add, ty: ValueType::F32, expected: 7.0f32.to_bits() as u64 },
        Case { case: "fold_f32_mul_consts", lhs: 3.0f32.to_bits() as u64, rhs: 4.0f32.to_bits() as u64, op: FloatBinaryOp::Mul, ty: ValueType::F32, expected: 12.0f32.to_bits() as u64 },
        Case { case: "fold_f32_div_consts", lhs: 10.0f32.to_bits() as u64, rhs: 4.0f32.to_bits() as u64, op: FloatBinaryOp::Div, ty: ValueType::F32, expected: 2.5f32.to_bits() as u64 },
        Case { case: "fold_f64_add_consts", lhs: 3.0f64.to_bits(), rhs: 4.0f64.to_bits(), op: FloatBinaryOp::Add, ty: ValueType::F64, expected: 7.0f64.to_bits() },
        Case { case: "fold_f64_mul_consts", lhs: 3.0f64.to_bits(), rhs: 4.0f64.to_bits(), op: FloatBinaryOp::Mul, ty: ValueType::F64, expected: 12.0f64.to_bits() },
        Case { case: "fold_f64_div_consts", lhs: 10.0f64.to_bits(), rhs: 4.0f64.to_bits(), op: FloatBinaryOp::Div, ty: ValueType::F64, expected: 2.5f64.to_bits() },
        Case { case: "fold_float_mul_by_one_identity", lhs: 2.5f64.to_bits(), rhs: 1.0f64.to_bits(), op: FloatBinaryOp::Mul, ty: ValueType::F64, expected: 2.5f64.to_bits() },
        Case { case: "fold_float_div_by_one_identity", lhs: 2.5f64.to_bits(), rhs: 1.0f64.to_bits(), op: FloatBinaryOp::Div, ty: ValueType::F64, expected: 2.5f64.to_bits() },
    ];
    for c in &cases {
        let mut fg = make_fn(|b| {
            let lhs = b.build_float_const(c.lhs, c.ty);
            let rhs = b.build_float_const(c.rhs, c.ty);
            b.build_float_binary_op(lhs, rhs, c.op, c.ty)
        })?;
        assert!(
            crate::pipeline::run_one(&ConstantFold::new(), &mut fg, &mut crate::OptCtx::new(None))?
                .changed(),
            "{}: ConstantFold must fold the two-const {:?}",
            c.case,
            c.op,
        );
        assert_eq!(
            return_kind(fg.graph())?,
            NodeKind::FloatConst(c.expected),
            "{}",
            c.case,
        );
    }
    Ok(())
}

/// Case table for the run-once two-const float compare folds (output is
/// the I1 boolean).  Each row names the stand-alone test it absorbed.
#[test]
fn fold_float_cmp_two_consts_cases() -> Result<()> {
    struct Case {
        case: &'static str,
        lhs: u64,
        rhs: u64,
        op: FloatCmpOp,
        ty: ValueType,
        expected: u64,
    }
    #[rustfmt::skip]
    let cases = [
        Case { case: "fold_f32_less_true", lhs: 3.0f32.to_bits() as u64, rhs: 4.0f32.to_bits() as u64, op: FloatCmpOp::Less, ty: ValueType::F32, expected: 1 },
        Case { case: "fold_f64_equal_true", lhs: 4.0f64.to_bits(), rhs: 4.0f64.to_bits(), op: FloatCmpOp::Equal, ty: ValueType::F64, expected: 1 },
        // NaN != NaN per IEEE 754.
        Case { case: "fold_f64_equal_nan_false", lhs: f64::NAN.to_bits(), rhs: f64::NAN.to_bits(), op: FloatCmpOp::Equal, ty: ValueType::F64, expected: 0 },
    ];
    for c in &cases {
        let mut fg = make_fn(|b| {
            let lhs = b.build_float_const(c.lhs, c.ty);
            let rhs = b.build_float_const(c.rhs, c.ty);
            b.build_float_cmp_op(lhs, rhs, c.op)
        })?;
        assert!(
            crate::pipeline::run_one(&ConstantFold::new(), &mut fg, &mut crate::OptCtx::new(None))?
                .changed(),
            "{}: ConstantFold must fold the two-const {:?}",
            c.case,
            c.op,
        );
        {
            let ret_val = return_value(fg.graph())?;
            assert!(
                fg.int_const_u128(ret_val) == Some(u128::from(c.expected)),
                "{}: expected IntConst({:#x}), got {:?}",
                c.case,
                c.expected,
                fg.int_const_u128(ret_val),
            );
        }
    }
    Ok(())
}

/// Case table for the run-once float unary constant folds.  Each row
/// names the stand-alone test it absorbed.
#[test]
fn fold_float_unary_const_cases() -> Result<()> {
    struct Case {
        case: &'static str,
        input: u64,
        op: FloatUnaryOp,
        ty: ValueType,
        expected: u64,
    }
    #[rustfmt::skip]
    let cases = [
        Case { case: "fold_f32_neg_const", input: 2.0f32.to_bits() as u64, op: FloatUnaryOp::Neg, ty: ValueType::F32, expected: (-2.0f32).to_bits() as u64 },
        Case { case: "fold_f64_abs_const", input: (-3.0f64).to_bits(), op: FloatUnaryOp::Abs, ty: ValueType::F64, expected: 3.0f64.to_bits() },
        Case { case: "fold_f64_sqrt_const", input: 4.0f64.to_bits(), op: FloatUnaryOp::Sqrt, ty: ValueType::F64, expected: 2.0f64.to_bits() },
    ];
    for c in &cases {
        let mut fg = make_fn(|b| {
            let v = b.build_float_const(c.input, c.ty);
            b.build_float_unary_op(v, c.op, c.ty)
        })?;
        assert!(
            crate::pipeline::run_one(&ConstantFold::new(), &mut fg, &mut crate::OptCtx::new(None))?
                .changed(),
            "{}: ConstantFold must fold {:?}",
            c.case,
            c.op,
        );
        assert_eq!(
            return_kind(fg.graph())?,
            NodeKind::FloatConst(c.expected),
            "{}",
            c.case,
        );
    }
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
            let v = b.build_float_const(input.to_bits(), ValueType::F64);
            b.build_float_unary_op(v, FloatUnaryOp::Round, ValueType::F64)
        })?;
        assert!(
            crate::pipeline::run_one(&ConstantFold::new(), &mut fg, &mut crate::OptCtx::new(None))?
                .changed(),
            "Round({input}) did not fold"
        );
        assert_eq!(
            return_kind(fg.graph())?,
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
            let v = b.build_float_const(input.to_bits() as u64, ValueType::F32);
            b.build_float_unary_op(v, FloatUnaryOp::Round, ValueType::F32)
        })?;
        assert!(
            crate::pipeline::run_one(&ConstantFold::new(), &mut fg, &mut crate::OptCtx::new(None))?
                .changed(),
            "Round({input}) did not fold"
        );
        assert_eq!(
            return_kind(fg.graph())?,
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
        let a = b.build_float_const(1.0f64.to_bits(), ValueType::F64);
        let b2 = b.build_float_const(2.0f64.to_bits(), ValueType::F64);
        let sum = b.build_float_binary_op(a, b2, FloatBinaryOp::Add, ValueType::F64)?;
        let as_int = b.build_float_bits_to_int(sum, ValueType::I64)?;
        let back_to_float = b.build_int_bits_to_float(as_int, ValueType::F64)?;
        Ok(back_to_float)
    })?;
    assert!(
        crate::pipeline::run_one(&ConstantFold::new(), &mut fg, &mut crate::OptCtx::new(None))?
            .changed()
    );
    // Float binary fold: sum → FloatConst(3.0).
    // Bitcast identity fold: IntBitsToFloat(FloatBitsToInt(FloatConst(3.0))) → FloatConst(3.0).
    assert_return_kind(fg.graph(), NodeKind::FloatConst(3.0f64.to_bits()));
    Ok(())
}

// (CastToFloat lowering tests removed: there is no CastToFloat node — an
// int→float cast is built directly as IntBitsToFloat / FloatConst, and a
// float→float cast as FloatToFloat, by `cast_to_float_if_needed` at build
// time.  That lowering is covered by the strider-ir builder tests.)

// ── Comprehensive tests ──────────────────────────────────────────────────────

/// A NaN-producing float fold is withheld: a NaN's bit pattern is
/// target-dependent, so `NaN + 1.0` is left as a `FloatAdd` rather than
/// folded to a host-specific NaN constant.
#[test]
fn fold_f64_nan_plus_one_is_not_folded() -> Result<()> {
    let nan = f64::NAN.to_bits();
    let mut fg = make_fn(|b| {
        let a = b.build_float_const(nan, ValueType::F64);
        let one = b.build_float_const(1.0f64.to_bits(), ValueType::F64);
        b.build_float_binary_op(a, one, FloatBinaryOp::Add, ValueType::F64)
    })?;
    // Nothing folds: the only op is the NaN-producing Add.
    assert!(
        !crate::pipeline::run_one(&ConstantFold::new(), &mut fg, &mut crate::OptCtx::new(None))?
            .changed()
    );
    let val = return_value(fg.graph())?;
    assert!(
        matches!(
            *fg.kind_of_value(val),
            NodeKind::FloatBinaryOp(FloatBinaryOp::Add)
        ),
        "NaN-producing Add must remain unfolded"
    );
    Ok(())
}

/// `inf - inf` is NaN per IEEE 754.  `Neg(inf)` still folds (a sign-bit
/// flip, `-inf`, not NaN), but the resulting `Add(inf, -inf)` is NaN, whose
/// bit pattern is target-dependent — so the Add is left unfolded.
#[test]
fn fold_f64_inf_minus_inf_add_is_not_folded() -> Result<()> {
    // `inf - inf` lowered to `Add(inf, Neg(inf))`.
    let inf = f64::INFINITY.to_bits();
    let mut fg = make_fn(|b| {
        let a = b.build_float_const(inf, ValueType::F64);
        let bb = b.build_float_const(inf, ValueType::F64);
        let neg_b = b.build_float_unary_op(bb, FloatUnaryOp::Neg, ValueType::F64)?;
        b.build_float_binary_op(a, neg_b, FloatBinaryOp::Add, ValueType::F64)
    })?;
    // `Neg(inf)` folds to the `-inf` constant, so the pass reports a change,
    // but the NaN-producing Add survives as the returned value's producer.
    assert!(
        crate::pipeline::run_one(&ConstantFold::new(), &mut fg, &mut crate::OptCtx::new(None))?
            .changed()
    );
    let val = return_value(fg.graph())?;
    assert!(
        matches!(
            *fg.kind_of_value(val),
            NodeKind::FloatBinaryOp(FloatBinaryOp::Add)
        ),
        "NaN-producing Add must remain unfolded"
    );
    Ok(())
}

/// Bitcast roundtrip on f32 — `IntBitsToFloat(FloatBitsToInt(non-const))` → non-const.
/// Uses a non-const float (the result of a float Add) so the builder doesn't
/// fold the inner cast eagerly.
#[test]
fn fold_bitcast_roundtrip_f32() -> Result<()> {
    let mut fg = make_fn(|b| {
        let a = b.build_float_const(1.0f32.to_bits() as u64, ValueType::F32);
        let b2 = b.build_float_const(1.5f32.to_bits() as u64, ValueType::F32);
        let sum = b.build_float_binary_op(a, b2, FloatBinaryOp::Add, ValueType::F32)?;
        let as_int = b.build_float_bits_to_int(sum, ValueType::I32)?;
        b.build_int_bits_to_float(as_int, ValueType::F32)
    })?;
    assert!(
        crate::pipeline::run_one(&ConstantFold::new(), &mut fg, &mut crate::OptCtx::new(None))?
            .changed()
    );
    // After folding: float Add → FloatConst(2.5), then bitcast roundtrip
    // collapses to that constant.
    assert_return_kind(fg.graph(), NodeKind::FloatConst(2.5f32.to_bits() as u64));
    Ok(())
}

/// Single-pass cascade: `((1 + 2) + 3) + 4` must converge to `IntConst(10)`
/// in a single `optimize()` call (i.e. without relying on the outer
/// pipeline fixed-point loop). Verifies that consumers are re-enqueued
/// into the worklist after a rule rewrites a node.
#[test]
fn single_pass_propagates_through_chain() -> Result<()> {
    let mut fg = make_fn(|b| {
        let one = b.build_int_const(1u64, ValueType::I32).unwrap();
        let two = b.build_int_const(2u64, ValueType::I32).unwrap();
        let three = b.build_int_const(3u64, ValueType::I32).unwrap();
        let four = b.build_int_const(4u64, ValueType::I32).unwrap();
        let c1 = b.build_int_binary_operation(one, two, IntBinaryOp::Add, ValueType::I32)?;
        let c2 = b.build_int_binary_operation(c1, three, IntBinaryOp::Add, ValueType::I32)?;
        b.build_int_binary_operation(c2, four, IntBinaryOp::Add, ValueType::I32)
    })?;

    // Single optimize() call — must converge without the outer pipeline loop.
    crate::pipeline::run_one(&ConstantFold::new(), &mut fg, &mut crate::OptCtx::new(None))?;

    {
        let ret_val = return_value(fg.graph())?;
        assert!(
            fg.int_const_u128(ret_val) == Some(10),
            "expected single-pass convergence to IntConst(10), got {:?}",
            fg.int_const_u128(ret_val)
        );
    }
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
            let one = b.build_int_const(1u64, ValueType::I64).unwrap();
            acc = b.build_sub_as_add_neg(acc, one, ValueType::I64)?;
        }
        Ok(acc)
    })?;
    run_to_fixed_point(&ConstantFold::new(), &mut fg)?;
    assert_sub_with_const(&fg, x, 10, ValueType::I64)?;
    Ok(())
}

/// Case table for `eval_int_binary`'s input-masking and signed-overflow
/// guards.  `expected == None` means the fold must skip.  Each row names
/// the stand-alone test (and arm) it absorbed.
#[test]
fn eval_int_binary_masking_and_overflow_cases() {
    use crate::opt::constant_fold::eval_int::eval_int_binary;

    struct Case {
        case: &'static str,
        op: IntBinaryOp,
        l: u128,
        r: u128,
        ty: ValueType,
        expected: Option<u128>,
    }
    #[rustfmt::skip]
    let cases = [
        // INT_MIN / -1 signed overflow must skip on every narrow signed
        // type -- same shape as the u64 case already guarded explicitly.
        Case { case: "sdiv_narrow_int_min_neg_one_skips (I32)", op: IntBinaryOp::Sdiv, l: 0x8000_0000, r: 0xFFFF_FFFF, ty: ValueType::I32, expected: None },
        Case { case: "sdiv_narrow_int_min_neg_one_skips (I16)", op: IntBinaryOp::Sdiv, l: 0x8000, r: 0xFFFF, ty: ValueType::I16, expected: None },
        Case { case: "sdiv_narrow_int_min_neg_one_skips (I8)", op: IntBinaryOp::Sdiv, l: 0x80, r: 0xFF, ty: ValueType::I8, expected: None },
        Case { case: "srem_narrow_int_min_neg_one_skips (I32)", op: IntBinaryOp::Srem, l: 0x8000_0000, r: 0xFFFF_FFFF, ty: ValueType::I32, expected: None },
        Case { case: "srem_narrow_int_min_neg_one_skips (I16)", op: IntBinaryOp::Srem, l: 0x8000, r: 0xFFFF, ty: ValueType::I16, expected: None },
        Case { case: "srem_narrow_int_min_neg_one_skips (I8)", op: IntBinaryOp::Srem, l: 0x80, r: 0xFF, ty: ValueType::I8, expected: None },
        // I8 Div with l carrying high garbage bits beyond I8.
        // Masked: 0xFF / 2 = 0x7F. Unmasked-eval: 0x1FF / 2 = 0xFF (wrong).
        Case { case: "eval_int_binary_unsigned_div_unmasked_u8", op: IntBinaryOp::Div, l: 0x1FF, r: 2, ty: ValueType::I8, expected: Some(0x7F) },
        // Pick a divisor that distinguishes the masked input from the raw
        // one: 0xFFFF % 7 = 1, 0x1FFFF % 7 = 5.
        Case { case: "eval_int_binary_unsigned_rem_unmasked_u16", op: IntBinaryOp::Rem, l: 0x1FFFF, r: 7, ty: ValueType::I16, expected: Some(1) },
        // Masked: 0xFF >> 1 = 0x7F. Unmasked-eval: 0x1FF >> 1 = 0xFF.
        Case { case: "eval_int_binary_unsigned_shr_unmasked_u8", op: IntBinaryOp::ShiftRight, l: 0x1FF, r: 1, ty: ValueType::I8, expected: Some(0x7F) },
    ];
    for c in &cases {
        assert_eq!(
            eval_int_binary(c.op, c.l, c.r, c.ty),
            c.expected,
            "{}",
            c.case,
        );
    }
}

/// Case table for `eval_int_cmp`'s input-masking guards (all at I8).
/// Each row names the stand-alone test it absorbed.
#[test]
fn eval_int_cmp_masking_cases() {
    use crate::opt::constant_fold::eval_int::eval_int_cmp;

    struct Case {
        case: &'static str,
        op: IntCmpOp,
        l: u128,
        r: u128,
        expected: bool,
        why: &'static str,
    }
    #[rustfmt::skip]
    let cases = [
        // Masked: 0xFF == 0xFF -> true. Unmasked-eval: 0x1FF != 0xFF -> false.
        Case { case: "eval_int_cmp_equal_unmasked_u8", op: IntCmpOp::Equal, l: 0x1FF, r: 0xFF, expected: true, why: "Equal must mask both sides to I8 before comparing" },
        // Masked: 0x00 < 0x01 -> true. Unmasked-eval: 0x100 < 0x01 -> false.
        Case { case: "eval_int_cmp_less_unmasked_u8", op: IntCmpOp::Less, l: 0x100, r: 0x01, expected: true, why: "Less must mask both sides to I8 before comparing" },
        // Masked: 0x00 + 0x00 -> no carry. Unmasked-eval: 0x100 + 0 = 0x100 > 0xFF -> false-carry.
        Case { case: "eval_int_cmp_carry_unmasked_u8", op: IntCmpOp::Carry, l: 0x100, r: 0, expected: false, why: "Carry must mask both sides before checking overflow" },
    ];
    for c in &cases {
        assert_eq!(
            eval_int_cmp(c.op, c.l, c.r, ValueType::I8).unwrap(),
            c.expected,
            "{}: {}",
            c.case,
            c.why,
        );
    }
}

/// Case table for `eval_int_cmp`'s `bits >= 128` arms (Scarry / Sborrow /
/// Carry at I128).  These wide-width branches use wrapping-add/sub +
/// sign-bit logic instead of the narrow `< min || > max` range check, and
/// were previously untested.
#[test]
fn eval_int_cmp_i128_overflow_arms_cases() {
    use crate::opt::constant_fold::eval_int::eval_int_cmp;

    struct Case {
        case: &'static str,
        op: IntCmpOp,
        l: u128,
        r: u128,
        expected: bool,
        why: &'static str,
    }
    #[rustfmt::skip]
    let cases = [
        // i128::MAX + 1 overflows: same-sign (both non-negative) inputs but a
        // negative wrapped result -> signed carry.
        Case { case: "scarry_i128_max_plus_one", op: IntCmpOp::Scarry, l: i128::MAX as u128, r: 1, expected: true, why: "i128::MAX + 1 signed-overflows" },
        // i128::MIN - 1 overflows: differing-sign inputs and a result whose
        // sign differs from the minuend -> signed borrow.
        Case { case: "sborrow_i128_min_minus_one", op: IntCmpOp::Sborrow, l: i128::MIN as u128, r: 1, expected: true, why: "i128::MIN - 1 signed-overflows" },
        // u128::MAX + 1 wraps to 0; the wrapped sum (0) < l (u128::MAX) is the
        // i128-width unsigned carry detector.
        Case { case: "carry_u128_max_plus_one", op: IntCmpOp::Carry, l: u128::MAX, r: 1, expected: true, why: "u128::MAX + 1 unsigned-overflows (sum wraps below l)" },
        // Control: no overflow on the same arms.
        Case { case: "scarry_i128_zero_no_overflow", op: IntCmpOp::Scarry, l: 0, r: 1, expected: false, why: "0 + 1 does not signed-overflow at I128" },
        Case { case: "sborrow_i128_zero_no_overflow", op: IntCmpOp::Sborrow, l: 0, r: 1, expected: false, why: "0 - 1 = -1 does not signed-overflow at I128 (both operands non-negative)" },
        Case { case: "carry_i128_zero_no_overflow", op: IntCmpOp::Carry, l: 0, r: 1, expected: false, why: "0 + 1 does not unsigned-overflow at I128" },
    ];
    for c in &cases {
        assert_eq!(
            eval_int_cmp(c.op, c.l, c.r, ValueType::I128).unwrap(),
            c.expected,
            "{}: {}",
            c.case,
            c.why,
        );
    }
}

/// Case table for `eval_int_binary`'s signed division / remainder positive
/// folds and the narrow signed-shift fill.  Only the by-zero / INT_MIN÷-1
/// skips were previously covered, so the value-producing signed arms went
/// untested.
#[test]
fn eval_int_binary_signed_value_folds_cases() {
    use crate::opt::constant_fold::eval_int::eval_int_binary;

    struct Case {
        case: &'static str,
        op: IntBinaryOp,
        l: u128,
        r: u128,
        ty: ValueType,
        expected: Option<u128>,
    }
    #[rustfmt::skip]
    let cases = [
        // Sdiv(-5, -2) at I8 = 2 (truncated toward zero); both operands
        // sign-extend before the i128 division.
        Case { case: "sdiv_neg_by_neg_i8", op: IntBinaryOp::Sdiv, l: 0xFB, r: 0xFE, ty: ValueType::I8, expected: Some(2) },
        // Srem(-7, 2) at I8 = -1 (0xFF); the remainder takes the dividend's
        // sign, then masks back to I8.
        Case { case: "srem_neg_by_pos_i8", op: IntBinaryOp::Srem, l: 0xF9, r: 2, ty: ValueType::I8, expected: Some(0xFF) },
        // SShiftRight(-128, 1) at I8 = -64 (0xC0); arithmetic shift fills the
        // vacated sign bit, NOT a logical zero-fill (which would give 0x40).
        Case { case: "sshr_neg_one_i8", op: IntBinaryOp::SShiftRight, l: 0x80, r: 1, ty: ValueType::I8, expected: Some(0xC0) },
    ];
    for c in &cases {
        assert_eq!(
            eval_int_binary(c.op, c.l, c.r, c.ty),
            c.expected,
            "{}",
            c.case,
        );
    }
}

// ── Bitwise-complement and two's-complement constant-fold semantics ────
//
// Bitwise complement (`~x`) is `Xor(x, all_ones)` — the canonical IR
// shape since the former BitNot unary-op was removed.  `Neg` is two's-complement
// negation (`-x`).  The MVN-based ARM `if_returns_const` lowering
// produces `Xor(IntConst(49), IntConst(all_ones))` which must fold to
// `~49 = -50`, not to `wrapping_neg(49) = -49`.

/// `IntUnaryOp::Neg` of `IntConst(50)` at I32 must fold to `-50`
/// (= 0xFFFF_FFCE = 4_294_967_246) — two's complement, NOT bitwise NOT.
#[test]
fn fold_int_unary_not_is_two_complement_u32() -> Result<()> {
    let mut fg = make_fn(|b| {
        let c = b.build_int_const(50u64, ValueType::I32).unwrap();
        b.build_int_unary_operation(c, IntUnaryOp::Neg, ValueType::I32)
    })?;
    assert!(
        crate::pipeline::run_one(&ConstantFold::new(), &mut fg, &mut crate::OptCtx::new(None))?
            .changed()
    );
    {
        let ret_val = return_value(fg.graph())?;
        assert!(
            fg.int_const_u128(ret_val) == Some(0xFFFF_FFCE),
            "IntUnaryOp::Neg(50) must fold to two's complement (=-50=0xFFFFFFCE), \
             not bitwise NOT (=~50=0xFFFFFFCD); got {:?}",
            fg.int_const_u128(ret_val)
        );
    }
    Ok(())
}

/// Two's complement of 0 is 0 — sanity check for the I64 path.
#[test]
fn fold_int_unary_not_zero_is_zero() -> Result<()> {
    let mut fg = make_fn(|b| {
        let c = b.build_int_const(0u64, ValueType::I64).unwrap();
        b.build_int_unary_operation(c, IntUnaryOp::Neg, ValueType::I64)
    })?;
    assert!(
        crate::pipeline::run_one(&ConstantFold::new(), &mut fg, &mut crate::OptCtx::new(None))?
            .changed()
    );
    assert_returns_const(&fg, 0);
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

/// Case table for `eval_int_binary`'s out-of-range (>= bit-width) shift
/// semantics.  Each row names the stand-alone test it absorbed; the `why`
/// keeps that test's Sleigh-divergence note.
#[test]
fn eval_int_binary_out_of_range_shift_cases() {
    use crate::opt::constant_fold::eval_int::eval_int_binary;

    struct Case {
        case: &'static str,
        op: IntBinaryOp,
        l: u128,
        r: u128,
        ty: ValueType,
        expected: Option<u128>,
        why: &'static str,
    }
    #[rustfmt::skip]
    let cases = [
        Case { case: "eval_int_binary_shl_at_bit_width_returns_zero_u32", op: IntBinaryOp::ShiftLeft, l: 1, r: 32, ty: ValueType::I32, expected: Some(0),
               why: "Sleigh: 1u32 << 32 = 0 (`r >= 8*sizeout` returns 0 per opbehavior.cc:411). Pre-fix fold computed `1 << (32 % 32) = 1 << 0 = 1` -- diverges from Sleigh." },
        Case { case: "eval_int_binary_shl_at_bit_width_returns_zero_u64", op: IntBinaryOp::ShiftLeft, l: 1, r: 64, ty: ValueType::I64, expected: Some(0),
               why: "Sleigh: 1u64 << 64 = 0.  Pre-fix fold computed `1 << (64 % 64) = 1`." },
        Case { case: "eval_int_binary_shl_above_bit_width_returns_zero_u32", op: IntBinaryOp::ShiftLeft, l: 0xFF, r: 40, ty: ValueType::I32, expected: Some(0),
               why: "Sleigh: shift > bit-width still returns 0.  Pre-fix fold computed `0xFF << (40 % 32) = 0xFF << 8 = 0xFF00`." },
        Case { case: "eval_int_binary_shr_at_bit_width_returns_zero_u32", op: IntBinaryOp::ShiftRight, l: 0xFFFF_FFFF, r: 32, ty: ValueType::I32, expected: Some(0),
               why: "Sleigh: 0xFFFFFFFFu32 >> 32 = 0 per opbehavior.cc:432.  Pre-fix fold computed `0xFFFFFFFF >> (32 % 32) = 0xFFFFFFFF`." },
        Case { case: "eval_int_binary_sshr_at_bit_width_negative_returns_all_ones_u32", op: IntBinaryOp::SShiftRight, l: 0xFFFF_FFFF, r: 32, ty: ValueType::I32, expected: Some(0xFFFF_FFFF),
               why: "Sleigh: signed-negative >> bit-width fills with the sign bit (= 0xFFFFFFFF) per opbehavior.cc:454-460." },
        Case { case: "eval_int_binary_sshr_at_bit_width_positive_returns_zero_u32", op: IntBinaryOp::SShiftRight, l: 0x7FFF_FFFF, r: 32, ty: ValueType::I32, expected: Some(0),
               why: "Sleigh: signed-non-negative >> bit-width = 0 (no sign bit to fill)." },
    ];
    for c in &cases {
        assert_eq!(
            eval_int_binary(c.op, c.l, c.r, c.ty),
            c.expected,
            "{}: {}",
            c.case,
            c.why,
        );
    }
}

// ── I128 interner-backed fold round-trip ──────────────────────────────────

/// `build_int_const(v, I128)` routes to the wide interner, and the
/// constant-fold pass can still fold an `Add` of two I128 constants
/// through the updated `int_const_u128` funnel.
#[test]
fn fold_i128_interner_backed_add_round_trip() -> Result<()> {
    // Values wider than u64, to ensure we exercise the interner path.
    let a: u128 = 1u128 << 100;
    let b_val: u128 = 1u128 << 101;
    let expected = a.wrapping_add(b_val);

    let mut fg = make_fn(|b| {
        let ca = b.build_int_const(a, ValueType::I128)?;
        let cb = b.build_int_const(b_val, ValueType::I128)?;
        b.build_int_binary_operation(ca, cb, IntBinaryOp::Add, ValueType::I128)
    })?;

    // Fold should fire: both operands are I128 int constants.
    let changed = crate::pipeline::run_one(&ConstantFold::new(), &mut fg, &mut crate::OptCtx::new(None))?
        .changed();
    assert!(
        changed,
        "ConstantFold must fold Add(I128, I128) to a constant"
    );

    // The result is readable through the int_const_u128 funnel.
    let ret_val = return_value(fg.graph())?;
    let folded = fg.int_const_u128(ret_val);
    assert_eq!(
        folded,
        Some(expected),
        "int_const_u128 must return the folded I128 sum via the interner funnel; \
         expected {expected:#x}, got {folded:?}",
    );
    Ok(())
}
