use super::*;
use anyhow::anyhow;
use strider_ir::UBig;
use strider_ir::node::{NodeKind, ValueType};
use strider_ir::{
    FloatBinaryOp, FloatCmpOp, FloatUnaryOp, IRBuilderExt, IRViewer, IRWalker, IntBinaryOp,
    IntCmpOp, IntUnaryOp,
};
use strider_ir_test_utils::IrBuilderEx;
use strider_ir_test_utils::IrWalkerEx;

use crate::test_support::{
    assert_return_kind, assert_returns_const, make_fn, make_fn_with_var, return_kind, return_value,
    run_to_fixed_point,
};
use strider_ir_test_utils::{RegisterSet, reg_vn};

fn add_consts_fixture() -> Result<strider_ir::Function> {
    make_fn(|b| {
        let c3 = b.build_int_const(3u64, ValueType::int(64)).unwrap();
        let c4 = b.build_int_const(4u64, ValueType::int(64)).unwrap();
        b.build_int_binary_operation(c3, c4, IntBinaryOp::Add, ValueType::int(64))
    })
}

/// The operand constants die in the fold, so the fresh `IntConst(7)` must
/// absorb their asm-fingerprints or their addresses are lost.
#[test]
fn const_eval_absorbs_operand_fingerprints() -> Result<()> {
    const A: u64 = 0xC0FF_EE01;
    const B: u64 = 0xC0FF_EE02;
    const ADD: u64 = 0xC0FF_EE03;
    let mut fg = make_fn(|b| {
        b.set_lift_addr(Some(A));
        let c3 = b.build_int_const(3u64, ValueType::int(64)).unwrap();
        b.set_lift_addr(Some(B));
        let c4 = b.build_int_const(4u64, ValueType::int(64)).unwrap();
        b.set_lift_addr(Some(ADD));
        b.build_int_binary_operation(c3, c4, IntBinaryOp::Add, ValueType::int(64))
    })?;

    crate::pipeline::run_one(&ConstantFold::new(), &mut fg, &crate::OptCtx::new(None))?;

    let folded = return_value(fg.graph())?;
    let fp = fg.side_tables().asm_fingerprint(fg.producer(folded));
    assert!(
        fp.contains(&A) && fp.contains(&B),
        "const-eval result must absorb both dying operand fingerprints \
         ({A:#x}, {B:#x}); got {fp:?}"
    );
    assert!(fp.contains(&ADD), "and the root's own {ADD:#x}; got {fp:?}");
    Ok(())
}

#[test]
fn two_independent_instances_each_fold() -> Result<()> {
    let pass_a = ConstantFold::new();
    let pass_b = ConstantFold::new();

    let mut fg_a = add_consts_fixture()?;
    assert!(crate::pipeline::run_one(&pass_a, &mut fg_a, &crate::OptCtx::new(None))?.changed());
    assert_returns_const(&fg_a, 7);

    let mut fg_b = add_consts_fixture()?;
    assert!(crate::pipeline::run_one(&pass_b, &mut fg_b, &crate::OptCtx::new(None))?.changed());
    assert_returns_const(&fg_b, 7);
    Ok(())
}

/// A shift count owes the output no width (`NodeKind::operand_width`), and
/// the evaluator tests it against the width before masking, so a count of any
/// width folds: one past the width shifts everything out.
/// Mutation: check every operand against the output width again.
#[test]
fn a_shift_count_of_another_width_folds() -> Result<()> {
    use strider_ir::IRBuilder;
    use strider_ir::node::ValueKind;
    let cases = [
        (
            IntBinaryOp::ShiftRight,
            0x8000_0000u64,
            4u64,
            8,
            0x0800_0000u128,
        ),
        (IntBinaryOp::ShiftLeft, 1, 33, 64, 0),
        (IntBinaryOp::SShiftRight, 0x8000_0000, 1, 1, 0xc000_0000),
    ];
    for (op, value, count, count_bits, want) in cases {
        let mut fg = make_fn(|b| {
            let lhs = b.build_int_const(value, ValueType::int(32))?;
            let count_ty = if count_bits == 1 {
                ValueType::I1
            } else {
                ValueType::int(count_bits)
            };
            let amt = b.build_int_const(count, count_ty)?;
            let node = b.create_node(
                NodeKind::IntBinaryOp(op),
                [lhs, amt],
                [ValueKind::Typed(ValueType::int(32))],
            );
            let [out] = b
                .node_outputs_exact::<1>(node)
                .expect("binary op has 1 output");
            Ok(out)
        })?;
        crate::pipeline::run_one(&ConstantFold::new(), &mut fg, &crate::OptCtx::new(None))?;
        let ret = return_value(fg.graph())?;
        assert_eq!(
            fg.int_const_u128(ret),
            Some(want),
            "{op:?} by {count}:i{count_bits}"
        );
    }
    Ok(())
}

/// The int-binary const-eval masks both operands to the output width. The
/// lifter never emits a width-mismatched node, but the validator types
/// `IntBinaryOp` inputs only as `AnyInt`, so the fold must skip rather than
/// evaluate against a silently re-masked value.
#[test]
fn int_binary_fold_skips_width_mismatched_operands() -> Result<()> {
    use strider_ir::node::ValueKind;
    // Masking the wide addend to the output width would drop its high bit.
    let mut fg = make_fn(|b| {
        let one = b.build_int_const(1u64, ValueType::int(64))?;
        let wide = b.build_int_const(0x1_0000_0001u64, ValueType::int(64))?;
        b.build_int_binary_operation(one, wide, IntBinaryOp::Add, ValueType::int(64))
    })?;
    let mut edit = crate::EditFunction::new(&mut fg);
    let add64 = return_value(edit.function().graph())?;
    let [_, wide] = edit.producer_inputs_exact::<2>(add64)?;
    let one = edit.build_int_const(1u64, ValueType::int(32))?;
    let add = edit.create_node(
        NodeKind::IntBinaryOp(IntBinaryOp::Add),
        [one, wide],
        [ValueKind::Typed(ValueType::int(32))],
    );
    let [narrow] = edit.node_outputs_exact::<1>(add)?;
    assert!(
        edit.replace_value(add64, narrow),
        "the return reads the narrow Add"
    );

    for (i, rule) in super::rules::build_rules().into_iter().enumerate() {
        assert!(
            rule(&mut edit, add)?.is_none(),
            "rule {i} folded an `Add` over a wider constant"
        );
    }
    Ok(())
}

/// `Neg` evaluates at its output width, so an operand of another width is not
/// that constant. `validate` rejects the node, but a rewrite can mint one
/// mid-pipeline, where nothing has validated yet.
#[test]
fn int_unary_fold_skips_a_width_mismatched_operand() -> Result<()> {
    use strider_ir::node::ValueKind;
    let mut fg = make_fn(|b| {
        let wide = b.build_int_const(0x1_0000_0001u64, ValueType::int(64))?;
        b.build_int_unary_operation(wide, IntUnaryOp::Neg, ValueType::int(64))
    })?;
    let mut edit = crate::EditFunction::new(&mut fg);
    let neg64 = return_value(edit.function().graph())?;
    let [wide] = edit.producer_inputs_exact::<1>(neg64)?;
    let neg = edit.create_node(
        NodeKind::IntUnaryOp(IntUnaryOp::Neg),
        [wide],
        [ValueKind::Typed(ValueType::int(32))],
    );
    let [narrow] = edit.node_outputs_exact::<1>(neg)?;
    assert!(
        edit.replace_value(neg64, narrow),
        "the return reads the narrow Neg"
    );

    for (i, rule) in super::rules::build_rules().into_iter().enumerate() {
        assert!(
            rule(&mut edit, neg)?.is_none(),
            "rule {i} folded a `Neg` over a wider constant"
        );
    }
    Ok(())
}

/// A foldable root nothing reads is left to the later rules: the fold
/// answers only when it redirected a use.
#[test]
fn const_eval_answers_nothing_for_a_root_without_uses() -> Result<()> {
    use strider_ir::node::ValueKind;
    let mut fg = make_fn(|b| b.build_int_const(1u64, ValueType::int(64)))?;
    let mut edit = crate::EditFunction::new(&mut fg);
    let c3 = edit.build_int_const(3u64, ValueType::int(64))?;
    let c4 = edit.build_int_const(4u64, ValueType::int(64))?;
    let add = edit.create_node(
        NodeKind::IntBinaryOp(IntBinaryOp::Add),
        [c3, c4],
        [ValueKind::Typed(ValueType::int(64))],
    );
    let [sum] = edit.node_outputs_exact::<1>(add)?;
    assert_eq!(edit.value_uses(sum).count(), 0, "nothing reads the Add");

    for (i, rule) in super::rules::build_rules().into_iter().enumerate() {
        assert!(
            rule(&mut edit, add)?.is_none(),
            "rule {i} answered for a root nothing reads"
        );
    }
    Ok(())
}

/// `validate` rejects a mixed-width comparison outright, which is what keeps
/// `eval::int_cmp` (which masks both operands to the LHS width, and so would
/// silently narrow a wider RHS) from ever seeing one.  Its own width guard
/// stays as defence against a rewrite minting one mid-pipeline, where no
/// `validate` runs.
#[test]
fn a_mixed_width_cmp_does_not_validate() {
    use strider_ir::IRBuilder;
    use strider_ir::node::ValueKind;
    let err = make_fn(|b| {
        let lhs = b.build_int_const(0x80u64, ValueType::int(8))?;
        let rhs = b.build_int_const(200u64, ValueType::int(32))?;
        let node = b.create_node(
            NodeKind::IntCmpOp(IntCmpOp::Sless),
            [lhs, rhs],
            [ValueKind::Typed(ValueType::I1)],
        );
        let [out] = b
            .node_outputs_exact::<1>(node)
            .expect("cmp op has 1 output");
        Ok(out)
    });
    let Err(err) = err else {
        panic!("a mixed-width comparison must not validate");
    };
    let msg = format!("{err:#}");
    assert!(msg.contains("mixes widths"), "got: {msg}");
}

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
        Case { case: "fold_int_add_consts", lhs: 3, rhs: 4, op: IntBinaryOp::Add, ty: ValueType::int(64), expected: 7 },
        Case { case: "fold_int_and_zero", lhs: 0xFF, rhs: 0, op: IntBinaryOp::And, ty: ValueType::int(64), expected: 0 },
        Case { case: "fold_mul_by_one", lhs: 5, rhs: 1, op: IntBinaryOp::Mul, ty: ValueType::int(64), expected: 5 },
        Case { case: "fold_shl_const_u32", lhs: 1, rhs: 4, op: IntBinaryOp::ShiftLeft, ty: ValueType::int(32), expected: 0x10 },
        Case { case: "fold_shr_const_u8", lhs: 0x80, rhs: 7, op: IntBinaryOp::ShiftRight, ty: ValueType::int(8), expected: 1 },
        // The Xor-with-all-ones rows are bitwise NOT, so `~49 = 0xFFFF_FFCE`,
        // NOT two's complement `-49`.
        Case { case: "fold_int_unary_neg_is_bitwise_not_u32", lhs: 49, rhs: u128::MAX, op: IntBinaryOp::Xor, ty: ValueType::int(32), expected: 0xFFFF_FFCE },
        Case { case: "fold_int_unary_neg_intermediate_is_bitwise_not_u8", lhs: 0xAA, rhs: u128::MAX, op: IntBinaryOp::Xor, ty: ValueType::int(8), expected: 0x55 },
        Case { case: "fold_int_unary_neg_zero_is_all_ones_u32", lhs: 0, rhs: u128::MAX, op: IntBinaryOp::Xor, ty: ValueType::int(32), expected: 0xFFFF_FFFF },
    ];
    for c in &cases {
        let mut fg = make_fn(|b| {
            let lhs = b.build_int_const(c.lhs, c.ty)?;
            let rhs = b.build_int_const(c.rhs, c.ty)?;
            b.build_int_binary_operation(lhs, rhs, c.op, c.ty)
        })?;
        assert!(
            crate::pipeline::run_one(&ConstantFold::new(), &mut fg, &crate::OptCtx::new(None))?
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

/// `Xor(C, C)` with ONE interned constant on both inputs folds to 0 through
/// the `x ^ x -> 0` identity, which runs before two-constant evaluation.
#[test]
fn fold_int_xor_of_one_constant_twice() -> Result<()> {
    let mut fg = make_fn(|b| {
        let x = b.build_int_const(0xABu64, ValueType::int(64)).unwrap();
        b.build_int_binary_operation(x, x, IntBinaryOp::Xor, ValueType::int(64))
    })?;
    assert!(
        crate::pipeline::run_one(&ConstantFold::new(), &mut fg, &crate::OptCtx::new(None))?
            .changed()
    );
    assert_returns_const(&fg, 0);
    Ok(())
}

/// As [`fold_int_xor_of_one_constant_twice`], over the lowered `Add(C, Neg(C))`.
#[test]
fn fold_int_sub_of_one_constant_twice() -> Result<()> {
    let mut fg = make_fn(|b| {
        let x = b.build_int_const(0xABu64, ValueType::int(64)).unwrap();
        b.build_sub_as_add_neg(x, x, ValueType::int(64))
    })?;
    assert!(
        crate::pipeline::run_one(&ConstantFold::new(), &mut fg, &crate::OptCtx::new(None))?
            .changed()
    );
    assert_returns_const(&fg, 0);
    Ok(())
}

#[test]
fn fold_add_zero_identity() -> Result<()> {
    let mut fg = make_fn(|b| {
        let c1 = b.build_int_const(1u64, ValueType::int(64)).unwrap();
        let c2 = b.build_int_const(2u64, ValueType::int(64)).unwrap();
        let x = b.build_int_binary_operation(c1, c2, IntBinaryOp::Add, ValueType::int(64))?;
        let zero = b.build_int_const(0u64, ValueType::int(64)).unwrap();
        b.build_int_binary_operation(x, zero, IntBinaryOp::Add, ValueType::int(64))
    })?;
    // `x + 0` collapses to x first, then x itself folds.
    run_to_fixed_point(&ConstantFold::new(), &mut fg)?;
    assert_returns_const(&fg, 3);
    Ok(())
}

/// `(x & 4) & 7`: bit 2 is the only bit reachable by both masks.
#[test]
fn fold_and_and_masks() -> Result<()> {
    let mut fg = make_fn(|b| {
        let x = b.build_int_const(0xFFu64, ValueType::int(64)).unwrap();
        let c4 = b.build_int_const(4u64, ValueType::int(64)).unwrap();
        let c7 = b.build_int_const(7u64, ValueType::int(64)).unwrap();
        let inner = b.build_int_binary_operation(x, c4, IntBinaryOp::And, ValueType::int(64))?;
        b.build_int_binary_operation(inner, c7, IntBinaryOp::And, ValueType::int(64))
    })?;
    run_to_fixed_point(&ConstantFold::new(), &mut fg)?;
    assert_returns_const(&fg, 4);
    Ok(())
}

/// Asserts the return value is `expected_base + expected_const`, type-masked,
/// operand order irrelevant.
fn assert_add_with_const(
    fg: &strider_ir::Function,
    expected_base: strider_ir::node::ValueId,
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
    let const_on = |o: strider_ir::node::ValueId| -> bool {
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

/// Asserts the return value is `expected_base - expected_const` in the
/// canonical post-fold shape `Add(base, IntConst(-K))`.
///
/// `IntBinaryOp::Sub` is not a primitive: pcode-lift produces `Add(a, Neg(b))`
/// and `ConstantFold` collapses `Neg(IntConst(K))` to `IntConst(-K)`, leaving
/// one `Add` with a negative-valued constant.
fn assert_sub_with_const(
    fg: &strider_ir::Function,
    expected_base: strider_ir::node::ValueId,
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
    // `Add` is commutative, so accept the const on either side.
    let l = inputs[0];
    let r = inputs[1];
    let neg_masked = ty
        .get_unsigned_int(u128::from(expected_const).wrapping_neg())
        .ok_or_else(|| anyhow!("expected integer type, got {ty:?}"))?;
    let const_match = |value: strider_ir::node::ValueId| {
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

/// `Neg` of an I256 const past the `u128` carrier folds at the declared
/// width: the result is `2^256 - v`, not anything a 128-bit carrier holds.
#[test]
fn unary_fold_folds_wide_const() -> Result<()> {
    let ty = ValueType::int(256);
    let v = UBig::from_words(&[1, 2, 3, 4]);
    let mut fg = make_fn(|b| {
        // Non-zero high limb, so the value cannot fit u128.
        let c = b.build_int_const(v.clone(), ty)?;
        b.build_int_unary_operation(c, IntUnaryOp::Neg, ty)
    })?;
    let outcome =
        crate::pipeline::run_one(&ConstantFold::new(), &mut fg, &crate::OptCtx::new(None))?;
    assert!(outcome.changed(), "wide-const unary fold must fold");
    let ret = return_value(fg.graph())?;
    assert_eq!(
        fg.int_const(ret),
        Some(&((UBig::ONE << 256usize) - v)),
        "Neg at I256 is the two's complement at 256 bits"
    );
    Ok(())
}

/// `x` is a register read, so it is genuinely non-const and the rule fires
/// without the `(C1, C2)` ping-pong the guard exists to prevent.
#[test]
fn canonicalize_commutative_const_to_right() -> Result<()> {
    let vn = reg_vn(0x1000, 8);
    let (mut fg, _x) = make_fn_with_var(vn, |b, x| {
        let c = b.build_int_const(5u64, ValueType::int(64)).unwrap();
        // Const on the LEFT.
        b.build_int_binary_operation(c, x, IntBinaryOp::Add, ValueType::int(64))
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
    // (x + 3) + 4 -> x + 7
    let vn = reg_vn(0x1000, 8);
    let (mut fg, x) = make_fn_with_var(vn, |b, x| {
        let c3 = b.build_int_const(3u64, ValueType::int(64)).unwrap();
        let c4 = b.build_int_const(4u64, ValueType::int(64)).unwrap();
        let inner = b.build_int_binary_operation(x, c3, IntBinaryOp::Add, ValueType::int(64))?;
        b.build_int_binary_operation(inner, c4, IntBinaryOp::Add, ValueType::int(64))
    })?;
    run_to_fixed_point(&ConstantFold::new(), &mut fg)?;
    assert_add_with_const(&fg, x, 7, ValueType::int(64))?;
    Ok(())
}

#[test]
fn reassoc_mul_mul_consts() -> Result<()> {
    // (x * 3) * 4 -> x * 12
    let vn = reg_vn(0x1000, 8);
    let (mut fg, x) = make_fn_with_var(vn, |b, x| {
        let c3 = b.build_int_const(3u64, ValueType::int(64)).unwrap();
        let c4 = b.build_int_const(4u64, ValueType::int(64)).unwrap();
        let inner = b.build_int_binary_operation(x, c3, IntBinaryOp::Mul, ValueType::int(64))?;
        b.build_int_binary_operation(inner, c4, IntBinaryOp::Mul, ValueType::int(64))
    })?;
    run_to_fixed_point(&ConstantFold::new(), &mut fg)?;
    assert_mul_with_const(&fg, x, 12, ValueType::int(64))
}

#[test]
fn reassoc_mul_mul_consts_wrap() -> Result<()> {
    // (x * (2^64 - 1)) * 3 -> x * (2^64 - 3)
    let vn = reg_vn(0x1000, 8);
    let (mut fg, x) = make_fn_with_var(vn, |b, x| {
        let big = b.build_int_const(u64::MAX, ValueType::int(64)).unwrap();
        let c3 = b.build_int_const(3u64, ValueType::int(64)).unwrap();
        let inner = b.build_int_binary_operation(x, big, IntBinaryOp::Mul, ValueType::int(64))?;
        b.build_int_binary_operation(inner, c3, IntBinaryOp::Mul, ValueType::int(64))
    })?;
    run_to_fixed_point(&ConstantFold::new(), &mut fg)?;
    assert_mul_with_const(&fg, x, u128::from(u64::MAX) - 2, ValueType::int(64))
}

#[test]
fn reassoc_add_sub_consts() -> Result<()> {
    // (x - 3) + 4 -> x + 1
    let vn = reg_vn(0x1000, 8);
    let (mut fg, x) = make_fn_with_var(vn, |b, x| {
        let c3 = b.build_int_const(3u64, ValueType::int(64)).unwrap();
        let c4 = b.build_int_const(4u64, ValueType::int(64)).unwrap();
        let inner = b.build_sub_as_add_neg(x, c3, ValueType::int(64))?;
        b.build_int_binary_operation(inner, c4, IntBinaryOp::Add, ValueType::int(64))
    })?;
    run_to_fixed_point(&ConstantFold::new(), &mut fg)?;
    assert_add_with_const(&fg, x, 1, ValueType::int(64))?;
    Ok(())
}

#[test]
fn reassoc_sub_add_consts_wrapping() -> Result<()> {
    // (x + 3) - 4 -> x + (3 - 4)  = x + 0xFFFF_FFFF_FFFF_FFFF
    let vn = reg_vn(0x1000, 8);
    let (mut fg, x) = make_fn_with_var(vn, |b, x| {
        let c3 = b.build_int_const(3u64, ValueType::int(64)).unwrap();
        let c4 = b.build_int_const(4u64, ValueType::int(64)).unwrap();
        let inner = b.build_int_binary_operation(x, c3, IntBinaryOp::Add, ValueType::int(64))?;
        b.build_sub_as_add_neg(inner, c4, ValueType::int(64))
    })?;
    run_to_fixed_point(&ConstantFold::new(), &mut fg)?;
    assert_add_with_const(&fg, x, 0xFFFF_FFFF_FFFF_FFFF, ValueType::int(64))?;
    Ok(())
}

#[test]
fn reassoc_sub_sub_consts() -> Result<()> {
    // (x - 3) - 4 -> x - 7
    let vn = reg_vn(0x1000, 8);
    let (mut fg, x) = make_fn_with_var(vn, |b, x| {
        let c3 = b.build_int_const(3u64, ValueType::int(64)).unwrap();
        let c4 = b.build_int_const(4u64, ValueType::int(64)).unwrap();
        let inner = b.build_sub_as_add_neg(x, c3, ValueType::int(64))?;
        b.build_sub_as_add_neg(inner, c4, ValueType::int(64))
    })?;
    run_to_fixed_point(&ConstantFold::new(), &mut fg)?;
    assert_sub_with_const(&fg, x, 7, ValueType::int(64))?;
    Ok(())
}

#[test]
fn reassoc_add_commuted_inner() -> Result<()> {
    // (3 + x) + 4 -> x + 7 (inner Add has const on lhs)
    let vn = reg_vn(0x1000, 8);
    let (mut fg, x) = make_fn_with_var(vn, |b, x| {
        let c3 = b.build_int_const(3u64, ValueType::int(64)).unwrap();
        let c4 = b.build_int_const(4u64, ValueType::int(64)).unwrap();
        let inner = b.build_int_binary_operation(c3, x, IntBinaryOp::Add, ValueType::int(64))?;
        b.build_int_binary_operation(inner, c4, IntBinaryOp::Add, ValueType::int(64))
    })?;
    run_to_fixed_point(&ConstantFold::new(), &mut fg)?;
    assert_add_with_const(&fg, x, 7, ValueType::int(64))?;
    Ok(())
}

#[test]
fn reassoc_add_commuted_outer() -> Result<()> {
    // 4 + (x + 3) -> x + 7 (outer Add has const on lhs)
    let vn = reg_vn(0x1000, 8);
    let (mut fg, x) = make_fn_with_var(vn, |b, x| {
        let c3 = b.build_int_const(3u64, ValueType::int(64)).unwrap();
        let c4 = b.build_int_const(4u64, ValueType::int(64)).unwrap();
        let inner = b.build_int_binary_operation(x, c3, IntBinaryOp::Add, ValueType::int(64))?;
        b.build_int_binary_operation(c4, inner, IntBinaryOp::Add, ValueType::int(64))
    })?;
    run_to_fixed_point(&ConstantFold::new(), &mut fg)?;
    assert_add_with_const(&fg, x, 7, ValueType::int(64))?;
    Ok(())
}

#[test]
fn reassoc_chain_three_subs() -> Result<()> {
    // ((x - 4) - 4) - 4 -> x - 12; needs the fixed-point loop to compose
    // several reassociation steps.
    let vn = reg_vn(0x1000, 8);
    let (mut fg, x) = make_fn_with_var(vn, |b, x| {
        let c4 = b.build_int_const(4u64, ValueType::int(64)).unwrap();
        let a = b.build_sub_as_add_neg(x, c4, ValueType::int(64))?;
        let b_ = b.build_sub_as_add_neg(a, c4, ValueType::int(64))?;
        b.build_sub_as_add_neg(b_, c4, ValueType::int(64))
    })?;
    run_to_fixed_point(&ConstantFold::new(), &mut fg)?;
    assert_sub_with_const(&fg, x, 12, ValueType::int(64))?;
    Ok(())
}

#[test]
fn reassoc_chain_three_subs_u32() -> Result<()> {
    // Same chain but at I32: ((x - 4) - 4) - 4 -> x - 12.
    let vn = reg_vn(0x1000, 4);
    let (mut fg, x) = make_fn_with_var(vn, |b, x| {
        let c4 = b.build_int_const(4u64, ValueType::int(32)).unwrap();
        let a = b.build_sub_as_add_neg(x, c4, ValueType::int(32))?;
        let b_ = b.build_sub_as_add_neg(a, c4, ValueType::int(32))?;
        b.build_sub_as_add_neg(b_, c4, ValueType::int(32))
    })?;
    run_to_fixed_point(&ConstantFold::new(), &mut fg)?;
    assert_sub_with_const(&fg, x, 12, ValueType::int(32))?;
    Ok(())
}

#[test]
fn reassoc_no_fold_without_const() -> Result<()> {
    // (x + y) + z, no constants -> untouched.
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
    let inner = b.build_int_binary_operation(x, y, IntBinaryOp::Add, ValueType::int(64))?;
    let outer = b.build_int_binary_operation(inner, z, IntBinaryOp::Add, ValueType::int(64))?;
    b.build_return(Some(outer), &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;
    let before = return_value(fg.graph())?;
    let res = crate::pipeline::run_one(&ConstantFold::new(), &mut fg, &crate::OptCtx::new(None))?;
    assert!(!res.changed(), "no-const chain should not reassociate");
    assert_eq!(return_value(fg.graph())?, before);
    Ok(())
}

/// Builds the AND-distribution LHS `((a & C1) | (b & C2)) & C3`.
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
    let k1 = b.build_int_const(c1, ValueType::int(64)).unwrap();
    let k2 = b.build_int_const(c2, ValueType::int(64)).unwrap();
    let k3 = b.build_int_const(c3, ValueType::int(64)).unwrap();
    let a_and = b.build_int_binary_operation(a, k1, IntBinaryOp::And, ValueType::int(64))?;
    let b_and = b.build_int_binary_operation(bval, k2, IntBinaryOp::And, ValueType::int(64))?;
    let or_node =
        b.build_int_binary_operation(a_and, b_and, IntBinaryOp::Or, ValueType::int(64))?;
    let outer = b.build_int_binary_operation(or_node, k3, IntBinaryOp::And, ValueType::int(64))?;
    b.build_return(Some(outer), &[])?;
    b.set_lift_addr(None);
    b.build()
}

/// Walks from entry, so detached zombies left behind by rewrites don't count.
fn reachable_or_nodes(fg: &strider_ir::Function) -> usize {
    fg.count_kind(|k| matches!(k, NodeKind::IntBinaryOp(IntBinaryOp::Or)))
}

/// The register-merge-mask case the AND-distribution rule exists for.
/// `C1 & C3 == 0`, so `(a & C1)` folds to 0, `Or(0, b & C2)` collapses, and one
/// `And` is left.
#[test]
fn distribution_rewrite_simplifies_when_a_product_is_zero() -> Result<()> {
    let mut fg = build_and_dist_fn(0xFFFF_0000, 0x0000_FFFF, 0x0000_FFFF)?;
    assert_eq!(reachable_or_nodes(&fg), 1, "test setup expects one Or node");
    let changed =
        crate::pipeline::run_one(&ConstantFold::new(), &mut fg, &crate::OptCtx::new(None))?
            .changed();
    assert!(changed, "distribution rule should fire and simplify");
    // Each `run_one` only re-seeds the directly-rewritten node, so the cascade
    // needs a few passes to settle.
    for _ in 0..8 {
        if !crate::pipeline::run_one(&ConstantFold::new(), &mut fg, &crate::OptCtx::new(None))?
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
    assert!(
        matches!(
            return_kind(fg.graph())?,
            NodeKind::IntBinaryOp(IntBinaryOp::And)
        ),
        "expected the returned value to be a single masking And",
    );
    Ok(())
}

/// The ARM/Thumb dispatch alignment idiom: `(x | 1) & 0xFFFF_FFFE` sets then
/// clears the Thumb bit, so the `Or` must drop and leave the jump-table
/// classifier a single masked load.
#[test]
fn align_or_removal_drops_or_when_set_bits_are_masked_off() -> Result<()> {
    let xv = reg_vn(0x1000, 8);
    let mut b = RegisterSet::new()
        .tracked(xv)
        .arg(xv)
        .build_fn_single_region()?;
    let x = b.read_variable(&xv)?;
    let one = b.build_int_const(1u64, ValueType::int(64))?;
    let mask = b.build_int_const(0xFFFF_FFFEu64, ValueType::int(64))?;
    let or_node = b.build_int_binary_operation(x, one, IntBinaryOp::Or, ValueType::int(64))?;
    let outer =
        b.build_int_binary_operation(or_node, mask, IntBinaryOp::And, ValueType::int(64))?;
    b.build_return(Some(outer), &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;
    assert_eq!(reachable_or_nodes(&fg), 1, "test setup expects one Or node");

    let changed =
        crate::pipeline::run_one(&ConstantFold::new(), &mut fg, &crate::OptCtx::new(None))?
            .changed();
    assert!(changed, "alignment-OR-removal rule should fire");
    for _ in 0..8 {
        if !crate::pipeline::run_one(&ConstantFold::new(), &mut fg, &crate::OptCtx::new(None))?
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

/// `(x | C1) & C2` reuses the LHS-bound `x` and `C2` at the root's width while
/// its disjointness test masks each constant to its OWN width, so a
/// width-mismatched shape decides `C1 & C2 == 0` in the wrong modulus.
/// `validate` rejects such a node on entry, so the shape is minted here the
/// way a mid-pipeline rewrite would.
#[test]
fn align_or_removal_skips_width_mismatched_operands() -> Result<()> {
    use strider_ir::node::ValueKind;
    use strider_ir::{EditFunction, IRBuilderExt};
    let xv = reg_vn(0x1000, 8);
    let mut b = RegisterSet::new()
        .tracked(xv)
        .arg(xv)
        .build_fn_single_region()?;
    let x = b.read_variable(&xv)?;
    let set_bit = b.build_int_const(0x100u64, ValueType::int(64))?;
    let or_value = b.build_int_binary_operation(x, set_bit, IntBinaryOp::Or, ValueType::int(64))?;
    let narrowed = b.truncate_if_needed(or_value, ValueType::int(8))?;
    b.build_return(Some(narrowed), &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    let mut edit = EditFunction::new(&mut fg);
    let mask = edit.build_int_const(0x7Fu64, ValueType::int(8))?;
    let and_node = edit.create_node(
        NodeKind::IntBinaryOp(IntBinaryOp::And),
        [or_value, mask],
        [ValueKind::Typed(ValueType::int(8))],
    );
    let [and_value] = edit
        .node_outputs_exact::<1>(and_node)
        .expect("binary op has 1 output");
    edit.replace_value(narrowed, and_value);

    for (i, rule) in super::rules::build_rules().into_iter().enumerate() {
        assert!(
            rule(&mut edit, and_node)?.is_none(),
            "rule {i} rewrote a width-mismatched `(x | C1) & C2`",
        );
    }
    Ok(())
}

/// With both products `C1 & C3` and `C2 & C3` non-zero the distribution is pure
/// churn: it pushes `& C3` inward, neither disjunct collapses, and the factored
/// shape regenerates. Unguarded, the term re-distributes forever. A second run
/// reporting no change is the fixed-point proof.
#[test]
fn distribution_does_not_churn_when_both_products_nonzero() -> Result<()> {
    // Both products are 0xFF.
    let mut fg = build_and_dist_fn(0xFF, 0xFF, 0xFF)?;
    assert_eq!(reachable_or_nodes(&fg), 1, "test setup expects one Or node");

    // Must reach the pass's internal fixed point without hanging.
    crate::pipeline::run_one(&ConstantFold::new(), &mut fg, &crate::OptCtx::new(None))?;

    assert_eq!(
        reachable_or_nodes(&fg),
        1,
        "the factored Or shape must be preserved (no churn)",
    );

    let second =
        crate::pipeline::run_one(&ConstantFold::new(), &mut fg, &crate::OptCtx::new(None))?
            .changed();
    assert!(
        !second,
        "result must be a stable fixed point (no further change)"
    );
    Ok(())
}

#[test]
fn fold_truncate_const() -> Result<()> {
    // `truncate_if_needed` folds inline, so the built graph has no Truncate
    // node at all, only an IntConst carrying the possibly-unmasked raw value.
    let fg = make_fn(|b| {
        let wide = b.build_int_const(0xFF00u64, ValueType::int(16)).unwrap();
        b.truncate_if_needed(wide, ValueType::int(8))
    })?;
    let val = return_value(fg.graph())?;
    // `int_const_u128` masks to the declared type.
    let semantic = fg.int_const_u128(val);
    assert_eq!(semantic, Some(0), "0xFF00 truncated to I8 should be 0");
    assert!(
        !fg.graph()
            .all_node_ids()
            .any(|n| matches!(fg.node_kind(n), NodeKind::Truncate)),
        "builder should have folded the truncate"
    );
    Ok(())
}

/// The truncate fold must mask the stored value to the output width, or the
/// IR invariant "an IntConst's stored value fits its declared type" breaks:
/// `Truncate(IntConst(0xFFFF, I16))` would become a typed-narrow but value-wide
/// `IntConst(0xFFFF, I8)`.
///
/// `truncate_if_needed` short-circuits constants, so `Truncate(IntConst)` can't
/// be emitted directly. Feeding the truncate a non-const Or that folds during
/// the optimizer loop gets the constant there instead.
#[test]
fn truncate_int_const_emits_masked_value() -> Result<()> {
    let mut fg = make_fn(|b| {
        let a = b.build_int_const(0xFFFFu64, ValueType::int(16)).unwrap();
        let b_ = b.build_int_const(0xFFFFu64, ValueType::int(16)).unwrap();
        // Non-const, so `truncate_if_needed` emits a real Truncate node.
        let or = b.build_int_binary_operation(a, b_, IntBinaryOp::Or, ValueType::int(16))?;
        b.truncate_if_needed(or, ValueType::int(8))
    })?;
    assert!(
        fg.graph()
            .all_node_ids()
            .any(|n| matches!(fg.node_kind(n), NodeKind::Truncate)),
        "test setup expects a Truncate node before optimization",
    );

    assert!(
        crate::pipeline::run_one(&ConstantFold::new(), &mut fg, &crate::OptCtx::new(None))?
            .changed()
    );

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

// The round-trip rules cover `Truncate(Extend(x))`, the shape `write_reg_vn`
// leaves behind.

use strider_ir::ExtendOp;

#[test]
fn fold_truncate_of_zero_extend_round_trip() -> Result<()> {
    let mut fg = make_fn(|b| {
        // Non-const I32 expression so the builder can't short-circuit.
        let a = b.build_int_const(0xAAu64, ValueType::int(32)).unwrap();
        let bb = b.build_int_const(0x55u64, ValueType::int(32)).unwrap();
        let or = b.build_int_binary_operation(a, bb, IntBinaryOp::Or, ValueType::int(32))?;
        let widened = b.extend_if_needed(or, ValueType::int(64), ExtendOp::ZeroExtend)?;
        b.truncate_if_needed(widened, ValueType::int(32))
    })?;
    run_to_fixed_point(&ConstantFold::new(), &mut fg)?;
    let val = return_value(fg.graph())?;
    assert!(
        matches!(fg.kind_of_value(val), NodeKind::IntConst(_)),
        "round-trip + const-fold must leave an IntConst at the root, got {:?}",
        fg.kind_of_value(val)
    );
    for nid in fg.walk() {
        let kind = fg.node_kind(nid);
        assert!(
            !matches!(kind, NodeKind::Truncate | NodeKind::Extend(_)),
            "Truncate/Extend round-trip must be folded away; found {kind:?}"
        );
    }
    Ok(())
}

#[test]
fn fold_nested_zero_extend_collapses() -> Result<()> {
    let xv = reg_vn(0x1000, 1); // I8 register
    let mut b = RegisterSet::new()
        .tracked(xv)
        .arg(xv)
        .build_fn_single_region()?;
    let x = b.read_variable(&xv)?;
    let w16 = b.extend_if_needed(x, ValueType::int(16), ExtendOp::ZeroExtend)?;
    let w32 = b.extend_if_needed(w16, ValueType::int(32), ExtendOp::ZeroExtend)?;
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

#[test]
fn fold_nested_sign_extend_collapses() -> Result<()> {
    let xv = reg_vn(0x1000, 1);
    let mut b = RegisterSet::new()
        .tracked(xv)
        .arg(xv)
        .build_fn_single_region()?;
    let x = b.read_variable(&xv)?;
    let w16 = b.extend_if_needed(x, ValueType::int(16), ExtendOp::SignExtend)?;
    let w32 = b.extend_if_needed(w16, ValueType::int(32), ExtendOp::SignExtend)?;
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

#[test]
fn fold_nested_truncate_collapses() -> Result<()> {
    let xv = reg_vn(0x1000, 4); // I32 register
    let mut b = RegisterSet::new()
        .tracked(xv)
        .arg(xv)
        .build_fn_single_region()?;
    let x = b.read_variable(&xv)?;
    let t16 = b.truncate_if_needed(x, ValueType::int(16))?;
    let t8 = b.truncate_if_needed(t16, ValueType::int(8))?;
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

#[test]
fn fold_truncate_of_sign_extend_round_trip() -> Result<()> {
    let mut fg = make_fn(|b| {
        // Non-const Or, so the rule fires through the inner expression rather
        // than via direct constant folding.
        let a = b.build_int_const(0x80u64, ValueType::int(32)).unwrap();
        let bb = b.build_int_const(0x01u64, ValueType::int(32)).unwrap();
        let or = b.build_int_binary_operation(a, bb, IntBinaryOp::Or, ValueType::int(32))?;
        let widened = b.extend_if_needed(or, ValueType::int(64), ExtendOp::SignExtend)?;
        b.truncate_if_needed(widened, ValueType::int(32))
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

/// The MIPS32 `mul a, b` shape: a 32x32->64 IntMul plus a 32-bit Truncate to
/// recover register width, which otherwise hides the Mul from the matcher's
/// data-flow walk for `add(mul(_,_), _)`.
#[test]
fn fold_narrow_mul_through_sign_extend() -> Result<()> {
    let mut fg = make_fn(|b| {
        let lhs = b.build_int_const(3u64, ValueType::int(32)).unwrap();
        let rhs = b.build_int_const(7u64, ValueType::int(32)).unwrap();
        // Non-const, so the constant folder can't collapse this before the
        // narrowing rule runs.
        let lhs_or = b.build_int_binary_operation(lhs, lhs, IntBinaryOp::Or, ValueType::int(32))?;
        let rhs_or = b.build_int_binary_operation(rhs, rhs, IntBinaryOp::Or, ValueType::int(32))?;
        let lhs_ext = b.extend_if_needed(lhs_or, ValueType::int(64), ExtendOp::SignExtend)?;
        let rhs_ext = b.extend_if_needed(rhs_or, ValueType::int(64), ExtendOp::SignExtend)?;
        let mul =
            b.build_int_binary_operation(lhs_ext, rhs_ext, IntBinaryOp::Mul, ValueType::int(64))?;
        b.truncate_if_needed(mul, ValueType::int(32))
    })?;
    run_to_fixed_point(&ConstantFold::new(), &mut fg)?;
    assert_returns_const(&fg, 21);
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

/// The x86 `mov $eax, ...` register-merge cleanup: the high-mask half
/// contributes nothing below bit 32, so the truncate drops it.
#[test]
fn fold_drop_high_half_in_or_truncate() -> Result<()> {
    let mut fg = make_fn(|b| {
        let low_part = b.build_int_const(0xAAu64, ValueType::int(64)).unwrap();
        let junk = b
            .build_int_const(0x12345678_DEADBEEFu64, ValueType::int(64))
            .unwrap();
        // Or-with-self makes low_part non-const so the rule fires through it.
        let low_or =
            b.build_int_binary_operation(low_part, low_part, IntBinaryOp::Or, ValueType::int(64))?;
        // Low 32 bits of the mask are zero.
        let high_mask = b
            .build_int_const(0xFFFFFFFF_00000000u64, ValueType::int(64))
            .unwrap();
        let high_part =
            b.build_int_binary_operation(high_mask, junk, IntBinaryOp::And, ValueType::int(64))?;
        let merged =
            b.build_int_binary_operation(low_or, high_part, IntBinaryOp::Or, ValueType::int(64))?;
        b.truncate_if_needed(merged, ValueType::int(32))
    })?;
    run_to_fixed_point(&ConstantFold::new(), &mut fg)?;
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

#[test]
fn fold_drop_low_mask_under_truncate() -> Result<()> {
    let mut fg = make_fn(|b| {
        // x is a non-const I64 expression.
        let a = b
            .build_int_const(0x1234_5678_DEAD_BEEFu64, ValueType::int(64))
            .unwrap();
        let x = b.build_int_binary_operation(a, a, IntBinaryOp::Or, ValueType::int(64))?;
        let low_mask = b
            .build_int_const(0xFFFFFFFFu64, ValueType::int(64))
            .unwrap();
        let masked =
            b.build_int_binary_operation(low_mask, x, IntBinaryOp::And, ValueType::int(64))?;
        b.truncate_if_needed(masked, ValueType::int(32))
    })?;
    run_to_fixed_point(&ConstantFold::new(), &mut fg)?;
    assert_returns_const(&fg, 0xDEADBEEF);
    Ok(())
}

/// The const-on-right And orientation. One rule orientation handles it: `x`
/// fails `any_int` structurally, so the `And`'s commutative retry binds
/// the const on either side.
#[test]
fn fold_drop_low_mask_under_truncate_const_on_right() -> Result<()> {
    let mut fg = make_fn(|b| {
        let a = b
            .build_int_const(0x1234_5678_DEAD_BEEFu64, ValueType::int(64))
            .unwrap();
        let x = b.build_int_binary_operation(a, a, IntBinaryOp::Or, ValueType::int(64))?;
        let low_mask = b
            .build_int_const(0xFFFFFFFFu64, ValueType::int(64))
            .unwrap();
        // Const on the RIGHT.
        let masked =
            b.build_int_binary_operation(x, low_mask, IntBinaryOp::And, ValueType::int(64))?;
        b.truncate_if_needed(masked, ValueType::int(32))
    })?;
    run_to_fixed_point(&ConstantFold::new(), &mut fg)?;
    assert_returns_const(&fg, 0xDEADBEEF);
    Ok(())
}

/// The And-term-on-left Or orientation. `low_part` fails the `and(...)`
/// subpattern structurally, so the `Or`'s commutative retry binds it on either
/// side.
#[test]
fn fold_drop_high_half_in_or_truncate_and_term_on_left() -> Result<()> {
    let mut fg = make_fn(|b| {
        let low_part = b.build_int_const(0xAAu64, ValueType::int(64)).unwrap();
        let junk = b
            .build_int_const(0x12345678_DEADBEEFu64, ValueType::int(64))
            .unwrap();
        let low_or =
            b.build_int_binary_operation(low_part, low_part, IntBinaryOp::Or, ValueType::int(64))?;
        let high_mask = b
            .build_int_const(0xFFFFFFFF_00000000u64, ValueType::int(64))
            .unwrap();
        let high_part =
            b.build_int_binary_operation(high_mask, junk, IntBinaryOp::And, ValueType::int(64))?;
        // And-term on the LEFT of the Or.
        let merged =
            b.build_int_binary_operation(high_part, low_or, IntBinaryOp::Or, ValueType::int(64))?;
        b.truncate_if_needed(merged, ValueType::int(32))
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

/// The round-trip rule must not fire when `x` is wider than the truncate's
/// output: `Truncate_U16(Extend_U64(x_U32))` is a real I32->I16 narrowing.
#[test]
fn fold_truncate_of_extend_skips_when_widths_differ() -> Result<()> {
    let mut fg = make_fn(|b| {
        let a = b.build_int_const(0xAAu64, ValueType::int(32)).unwrap();
        let bb = b.build_int_const(0x55u64, ValueType::int(32)).unwrap();
        let or = b.build_int_binary_operation(a, bb, IntBinaryOp::Or, ValueType::int(32))?;
        let widened = b.extend_if_needed(or, ValueType::int(64), ExtendOp::ZeroExtend)?;
        // I16 is narrower than the inner Or's I32, so the round-trip rule must
        // not fire. Constant-fold may still collapse the const-Or.
        b.truncate_if_needed(widened, ValueType::int(16))
    })?;
    run_to_fixed_point(&ConstantFold::new(), &mut fg)?;
    let val = return_value(fg.graph())?;
    assert_eq!(
        fg.value_kind(val),
        strider_ir::node::ValueKind::Typed(ValueType::int(16)),
        "Truncate_U16(Extend_U64(I32)) must keep I16 typing: the round-trip \
         rule must not fire when inner width != outer truncate width"
    );
    Ok(())
}

/// A truncate of an extend to a width other than the source's is one cast:
/// below the source's width a truncate of the source, above it the same
/// extend of the source. Both extends of every 8-bit source, against the
/// reference interpreter: `ext(x)` truncated to 16, 32 or 1 bits, then
/// extended again or not.
/// Mutation: drop the four truncate-of-extend rules.
#[test]
fn a_truncate_of_an_extend_is_one_cast() -> Result<()> {
    use strider_ir_test_utils::interp;
    let int = |w: u16| {
        if w == 1 {
            ValueType::I1
        } else {
            ValueType::int(w)
        }
    };
    let vn = reg_vn(0x1000, 1);
    // (extend to, truncate to, extend again to, casts left)
    let shapes = [
        (32, 16, None, 1),
        (64, 32, Some(64), 1),
        (32, 1, None, 1),
        (64, 1, Some(8), 2),
    ];
    for op in [ExtendOp::ZeroExtend, ExtendOp::SignExtend] {
        for (wide, narrow, again, casts) in shapes {
            let (raw, _) = make_fn_with_var(vn, |b, x| {
                let w = b.extend_if_needed(x, int(wide), op)?;
                let t = b.truncate_if_needed(w, int(narrow))?;
                match again {
                    Some(to) => b.extend_if_needed(t, int(to), op),
                    None => Ok(t),
                }
            })?;
            let mut opt = raw.clone();
            run_to_fixed_point(&ConstantFold::new(), &mut opt)?;
            let left = opt
                .walk()
                .filter(|&n| matches!(opt.node_kind(n), NodeKind::Truncate | NodeKind::Extend(_)))
                .count();
            assert_eq!(
                left,
                casts,
                "{op:?} {wide} {narrow} {again:?}\n{}",
                opt.to_text(false)
            );
            for x in 0..=0xffu128 {
                let env = interp::Env {
                    entry: &|_| x,
                    rom: None,
                    seed: 0,
                    step_budget: 100,
                    private: None,
                };
                let want = interp::run(&raw, &env);
                assert!(
                    matches!(&want, Ok(interp::Outcome::Return { values, .. })
                        if values.iter().all(|v| matches!(v, interp::Slot::Known(_)))),
                    "{want:?}"
                );
                assert_eq!(
                    interp::run(&opt, &env),
                    want,
                    "{op:?} {wide} {narrow} {again:?} at x={x:#x}"
                );
            }
        }
    }
    Ok(())
}

// Booleans are 1-bit integers: NOT is `Xor(x, IntConst(1)):I1`, AND/OR/XOR are
// `IntBinaryOp` at I1, and a bool const is `IntConst(0|1)` typed I1. So these
// folds all flow through the generic integer rules.

#[test]
fn fold_bool_and_consts() -> Result<()> {
    let mut fg = make_fn(|b| {
        let t = b.build_boolean_const(true);
        let f = b.build_boolean_const(false);
        b.build_int_binary_operation(t, f, IntBinaryOp::And, ValueType::I1)
    })?;
    assert!(
        crate::pipeline::run_one(&ConstantFold::new(), &mut fg, &crate::OptCtx::new(None))?
            .changed()
    );
    assert_returns_const(&fg, 0);
    Ok(())
}

// `xor b true` is already the canonical logical-NOT shape, so nothing should
// rewrite it when `b` is non-const.
#[test]
fn fold_bool_xor_true_to_not() -> Result<()> {
    let vn = reg_vn(0x1000, 8);
    let (mut fg, _x) = make_fn_with_var(vn, |b, x| {
        let c5 = b.build_int_const(5u64, ValueType::int(64)).unwrap();
        // Non-const bool.
        let cmp = b.build_int_cmp_operation(x, c5, IntCmpOp::Equal, ValueType::int(64))?;
        let t = b.build_boolean_const(true);
        b.build_int_binary_operation(cmp, t, IntBinaryOp::Xor, ValueType::I1)
    })?;
    // The run may or may not report a change; only the final shape matters.
    let _ = crate::pipeline::run_one(&ConstantFold::new(), &mut fg, &crate::OptCtx::new(None))?;
    assert_return_kind(fg.graph(), NodeKind::IntBinaryOp(IntBinaryOp::Xor));
    Ok(())
}

// `xor` is commutative, so `true ^ b` must behave as `b ^ true`.
#[test]
fn fold_bool_true_xor_x_to_not_commutative() -> Result<()> {
    let vn = reg_vn(0x1000, 8);
    let (mut fg, _x) = make_fn_with_var(vn, |b, x| {
        let c5 = b.build_int_const(5u64, ValueType::int(64)).unwrap();
        let cmp = b.build_int_cmp_operation(x, c5, IntCmpOp::Equal, ValueType::int(64))?;
        let t = b.build_boolean_const(true);
        b.build_int_binary_operation(t, cmp, IntBinaryOp::Xor, ValueType::I1)
    })?;
    let _ = crate::pipeline::run_one(&ConstantFold::new(), &mut fg, &crate::OptCtx::new(None))?;
    assert_return_kind(fg.graph(), NodeKind::IntBinaryOp(IntBinaryOp::Xor));
    Ok(())
}

// The const is 0, not all-ones, so `x ^ 0 -> x` fires and collapses the Xor to
// the cmp.
#[test]
fn no_fold_bool_xor_false() -> Result<()> {
    let vn = reg_vn(0x1000, 8);
    let (mut fg, _x) = make_fn_with_var(vn, |b, x| {
        let c5 = b.build_int_const(5u64, ValueType::int(64)).unwrap();
        let cmp = b.build_int_cmp_operation(x, c5, IntCmpOp::Equal, ValueType::int(64))?;
        let f = b.build_boolean_const(false);
        b.build_int_binary_operation(cmp, f, IntBinaryOp::Xor, ValueType::I1)
    })?;
    assert!(
        crate::pipeline::run_one(&ConstantFold::new(), &mut fg, &crate::OptCtx::new(None))?
            .changed()
    );
    assert_return_kind(fg.graph(), NodeKind::IntCmpOp(IntCmpOp::Equal));
    Ok(())
}

// `x | true -> true` at I1, where `true` is the all-ones value.
#[test]
fn fold_bool_or_true_to_true() -> Result<()> {
    let vn = reg_vn(0x1000, 8);
    let (mut fg, _x) = make_fn_with_var(vn, |b, x| {
        let c5 = b.build_int_const(5u64, ValueType::int(64)).unwrap();
        // Non-const bool.
        let cmp = b.build_int_cmp_operation(x, c5, IntCmpOp::Equal, ValueType::int(64))?;
        let t = b.build_boolean_const(true);
        b.build_int_binary_operation(cmp, t, IntBinaryOp::Or, ValueType::I1)
    })?;
    assert!(
        crate::pipeline::run_one(&ConstantFold::new(), &mut fg, &crate::OptCtx::new(None))?
            .changed()
    );
    // Folds to 1, not to the cmp.
    assert_returns_const(&fg, 1);
    Ok(())
}

// The Or-absorbing rule applies at any width, on any all-ones constant.
#[test]
fn fold_int_or_all_ones_to_all_ones() -> Result<()> {
    let vn = reg_vn(0x1000, 4); // 4-byte var -> I32
    let (mut fg, _x) = make_fn_with_var(vn, |b, x| {
        // `Neg` makes the value non-const.
        let x32 = b.build_int_unary_operation(x, IntUnaryOp::Neg, ValueType::int(32))?;
        let all_ones = b
            .build_int_const(0xFFFF_FFFFu64, ValueType::int(32))
            .unwrap();
        b.build_int_binary_operation(x32, all_ones, IntBinaryOp::Or, ValueType::int(32))
    })?;
    assert!(
        crate::pipeline::run_one(&ConstantFold::new(), &mut fg, &crate::OptCtx::new(None))?
            .changed()
    );
    assert_returns_const(&fg, 0xFFFF_FFFF);
    Ok(())
}

// Each logical NOT at I1 is `Xor(_, IntConst(1))`, so this builds
// `Xor(Xor(cmp, 1), 1)` for the double-negation rule to collapse.
#[test]
fn fold_bool_double_not_to_x() -> Result<()> {
    let vn = reg_vn(0x1000, 8);
    let (mut fg, _x) = make_fn_with_var(vn, |b, x| {
        let c5 = b.build_int_const(5u64, ValueType::int(64)).unwrap();
        let cmp = b.build_int_cmp_operation(x, c5, IntCmpOp::Equal, ValueType::int(64))?;
        let one = b.build_int_const(u128::MAX, ValueType::I1)?;
        let n1 = b.build_int_binary_operation(cmp, one, IntBinaryOp::Xor, ValueType::I1)?;
        b.build_int_binary_operation(n1, one, IntBinaryOp::Xor, ValueType::I1)
    })?;
    assert!(
        crate::pipeline::run_one(&ConstantFold::new(), &mut fg, &crate::OptCtx::new(None))?
            .changed()
    );
    assert_return_kind(fg.graph(), NodeKind::IntCmpOp(IntCmpOp::Equal));
    Ok(())
}

#[test]
fn no_fold_div_by_zero() -> Result<()> {
    let mut fg = make_fn(|b| {
        let x = b.build_int_const(10u64, ValueType::int(64)).unwrap();
        let zero = b.build_int_const(0u64, ValueType::int(64)).unwrap();
        b.build_int_binary_operation(x, zero, IntBinaryOp::Div, ValueType::int(64))
    })?;
    assert!(
        !crate::pipeline::run_one(&ConstantFold::new(), &mut fg, &crate::OptCtx::new(None))?
            .changed()
    );
    assert!(matches!(
        return_kind(fg.graph())?,
        NodeKind::IntBinaryOp(IntBinaryOp::Div)
    ));
    Ok(())
}

/// Sleigh leaves division by zero undefined, so every divide-family op must
/// skip rather than fold.
#[test]
fn no_fold_divide_family_by_zero_i32_cases() -> Result<()> {
    for op in [
        IntBinaryOp::Div,
        IntBinaryOp::Sdiv,
        IntBinaryOp::Rem,
        IntBinaryOp::Srem,
    ] {
        let mut fg = make_fn(|b| {
            let x = b.build_int_const(10u64, ValueType::int(32)).unwrap();
            let zero = b.build_int_const(0u64, ValueType::int(32)).unwrap();
            b.build_int_binary_operation(x, zero, op, ValueType::int(32))
        })?;
        assert!(
            !crate::pipeline::run_one(&ConstantFold::new(), &mut fg, &crate::OptCtx::new(None))?
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

/// Shifts by `bit_width - 1`, the last in-range amount, at I32.
#[test]
fn fold_shift_by_width_minus_one_cases() -> Result<()> {
    #[rustfmt::skip]
    let cases = [
        ("shl_by_31", IntBinaryOp::ShiftLeft, 1u128, 0x8000_0000u128),
        ("lshr_by_31", IntBinaryOp::ShiftRight, 0xFFFF_FFFF, 1),
        // Sign fill, not zero fill.
        ("ashr_by_31_negative", IntBinaryOp::SShiftRight, 0x8000_0000, 0xFFFF_FFFF),
        ("ashr_by_31_positive", IntBinaryOp::SShiftRight, 0x7FFF_FFFF, 0),
    ];
    for (case, op, lhs, expected) in cases {
        let mut fg = make_fn(|b| {
            let l = b.build_int_const(lhs, ValueType::int(32))?;
            let s = b.build_int_const(31u64, ValueType::int(32))?;
            b.build_int_binary_operation(l, s, op, ValueType::int(32))
        })?;
        assert!(
            crate::pipeline::run_one(&ConstantFold::new(), &mut fg, &crate::OptCtx::new(None))?
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

/// Arithmetic at the 1-bit width, where `Add(1, 1)` wraps to 0 because the
/// result is masked to the declared width.
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
        // An `op(c, c)` identity can fire instead of the two-const eval; the
        // result is the same masked constant.
        assert!(
            crate::pipeline::run_one(&ConstantFold::new(), &mut fg, &crate::OptCtx::new(None))?
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
        let c5 = b.build_int_const(5u64, ValueType::int(64)).unwrap();
        let c5b = b.build_int_const(5u64, ValueType::int(64)).unwrap();
        b.build_int_cmp_operation(c5, c5b, IntCmpOp::Equal, ValueType::int(64))
    })?;
    assert!(
        crate::pipeline::run_one(&ConstantFold::new(), &mut fg, &crate::OptCtx::new(None))?
            .changed()
    );
    assert_returns_const(&fg, 1);
    Ok(())
}

#[test]
fn fold_int_cmp_less_consts() -> Result<()> {
    let mut fg = make_fn(|b| {
        let c3 = b.build_int_const(3u64, ValueType::int(64)).unwrap();
        let c5 = b.build_int_const(5u64, ValueType::int(64)).unwrap();
        b.build_int_cmp_operation(c3, c5, IntCmpOp::Less, ValueType::int(64))
    })?;
    assert!(
        crate::pipeline::run_one(&ConstantFold::new(), &mut fg, &crate::OptCtx::new(None))?
            .changed()
    );
    assert_returns_const(&fg, 1);
    Ok(())
}

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
        Case { case: "fold_popcount_const", op: CountOp::Popcount, input: 0b1011_0101, ty: ValueType::int(8), expected: 5 },
        Case { case: "fold_popcount_zero", op: CountOp::Popcount, input: 0, ty: ValueType::int(64), expected: 0 },
        Case { case: "fold_lzcount_msb_set", op: CountOp::Lzcount, input: 0x80, ty: ValueType::int(8), expected: 0 },
        Case { case: "fold_lzcount_one", op: CountOp::Lzcount, input: 1, ty: ValueType::int(8), expected: 7 },
        // A zero input counts the TYPE's bit width, not the host's.
        Case { case: "fold_lzcount_zero_u32", op: CountOp::Lzcount, input: 0, ty: ValueType::int(32), expected: 32 },
        Case { case: "fold_lzcount_zero_u8", op: CountOp::Lzcount, input: 0, ty: ValueType::int(8), expected: 8 },
        Case { case: "fold_lzcount_zero_u64", op: CountOp::Lzcount, input: 0, ty: ValueType::int(64), expected: 64 },
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
            crate::pipeline::run_one(&ConstantFold::new(), &mut fg, &crate::OptCtx::new(None))?
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

/// Returns a function whose Return value is `kind(wide_const)`.
///
/// The normal unary builders can't produce this: `build_lzcount` /
/// `build_popcount` would coerce the input down to I64, collapsing the wide
/// const before the node sees it.
fn build_unary_with_wide_const_input(
    kind: NodeKind,
    wide_ty: ValueType,
    out_ty: ValueType,
) -> Result<strider_ir::Function> {
    use strider_ir::node::ValueKind;
    let mut fg = make_fn(|b| Ok(b.build_int_const(0u64, ValueType::int(64)).unwrap()))?;
    let placeholder = return_value(fg.graph())?;
    let const_id = fg.intern_int_const(0xFF_u128, wide_ty);
    let wide_node = fg.graph_mut().create_node(
        NodeKind::IntConst(const_id),
        [],
        [ValueKind::Typed(wide_ty)],
    );
    // The validator requires an asm fingerprint on every node.
    fg.side_tables_mut()
        .extend_asm_fingerprint(wide_node, &[strider_ir_test_utils::SENTINEL_LIFT_ADDR]);
    let wide_const = fg.node_outputs_exact::<1>(wide_node)?[0];
    let unary_node = fg
        .graph_mut()
        .create_node(kind, [wide_const], [ValueKind::Typed(out_ty)]);
    fg.side_tables_mut()
        .extend_asm_fingerprint(unary_node, &[strider_ir_test_utils::SENTINEL_LIFT_ADDR]);
    let unary_value = fg.node_outputs_exact::<1>(unary_node)?[0];
    fg.graph_mut().replace_all_uses(placeholder, unary_value);
    Ok(fg)
}

/// `eval::popcount` / `eval::lzcount` count within the input's declared
/// width, so a const input past 128 bits folds, counting over all of it.
/// The input is `0xFF`.
#[test]
fn fold_count_op_wide_const_input_cases() -> Result<()> {
    #[rustfmt::skip]
    let cases = [
        ("fold_lzcount_u128_input", NodeKind::Lzcount, ValueType::int(128), 120u64),
        ("fold_lzcount_u256_input", NodeKind::Lzcount, ValueType::int(256), 248),
        ("fold_popcount_u128_input", NodeKind::Popcount, ValueType::int(128), 8),
        ("fold_popcount_u256_input", NodeKind::Popcount, ValueType::int(256), 8),
    ];
    for (case, kind, wide_ty, expected) in cases {
        let mut fg = build_unary_with_wide_const_input(kind, wide_ty, ValueType::int(64))?;
        let outcome =
            crate::pipeline::run_one(&ConstantFold::new(), &mut fg, &crate::OptCtx::new(None))?;
        assert!(
            outcome.changed(),
            "{case}: ConstantFold must fold {kind:?}({wide_ty:?} const)"
        );
        assert_eq!(
            fg.int_const_u128(return_value(fg.graph())?),
            Some(u128::from(expected)),
            "{case}: counted over the declared width"
        );
    }
    Ok(())
}

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
        Case { case: "fold_f32_add_consts", lhs: 3.0f32.to_bits() as u64, rhs: 4.0f32.to_bits() as u64, op: FloatBinaryOp::Add, ty: ValueType::float(32), expected: 7.0f32.to_bits() as u64 },
        Case { case: "fold_f32_mul_consts", lhs: 3.0f32.to_bits() as u64, rhs: 4.0f32.to_bits() as u64, op: FloatBinaryOp::Mul, ty: ValueType::float(32), expected: 12.0f32.to_bits() as u64 },
        Case { case: "fold_f32_div_consts", lhs: 10.0f32.to_bits() as u64, rhs: 4.0f32.to_bits() as u64, op: FloatBinaryOp::Div, ty: ValueType::float(32), expected: 2.5f32.to_bits() as u64 },
        Case { case: "fold_f64_add_consts", lhs: 3.0f64.to_bits(), rhs: 4.0f64.to_bits(), op: FloatBinaryOp::Add, ty: ValueType::float(64), expected: 7.0f64.to_bits() },
        Case { case: "fold_f64_mul_consts", lhs: 3.0f64.to_bits(), rhs: 4.0f64.to_bits(), op: FloatBinaryOp::Mul, ty: ValueType::float(64), expected: 12.0f64.to_bits() },
        Case { case: "fold_f64_div_consts", lhs: 10.0f64.to_bits(), rhs: 4.0f64.to_bits(), op: FloatBinaryOp::Div, ty: ValueType::float(64), expected: 2.5f64.to_bits() },
        Case { case: "fold_float_mul_by_one_identity", lhs: 2.5f64.to_bits(), rhs: 1.0f64.to_bits(), op: FloatBinaryOp::Mul, ty: ValueType::float(64), expected: 2.5f64.to_bits() },
        Case { case: "fold_float_div_by_one_identity", lhs: 2.5f64.to_bits(), rhs: 1.0f64.to_bits(), op: FloatBinaryOp::Div, ty: ValueType::float(64), expected: 2.5f64.to_bits() },
    ];
    for c in &cases {
        let mut fg = make_fn(|b| {
            let lhs = b.build_float_const(c.lhs, c.ty);
            let rhs = b.build_float_const(c.rhs, c.ty);
            b.build_float_binary_op(lhs, rhs, c.op, c.ty)
        })?;
        assert!(
            crate::pipeline::run_one(&ConstantFold::new(), &mut fg, &crate::OptCtx::new(None))?
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

/// Float compares output the I1 boolean, so the assertions read IntConst.
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
        Case { case: "fold_f32_less_true", lhs: 3.0f32.to_bits() as u64, rhs: 4.0f32.to_bits() as u64, op: FloatCmpOp::Less, ty: ValueType::float(32), expected: 1 },
        Case { case: "fold_f64_equal_true", lhs: 4.0f64.to_bits(), rhs: 4.0f64.to_bits(), op: FloatCmpOp::Equal, ty: ValueType::float(64), expected: 1 },
        // NaN != NaN per IEEE 754.
        Case { case: "fold_f64_equal_nan_false", lhs: f64::NAN.to_bits(), rhs: f64::NAN.to_bits(), op: FloatCmpOp::Equal, ty: ValueType::float(64), expected: 0 },
    ];
    for c in &cases {
        let mut fg = make_fn(|b| {
            let lhs = b.build_float_const(c.lhs, c.ty);
            let rhs = b.build_float_const(c.rhs, c.ty);
            b.build_float_cmp_op(lhs, rhs, c.op)
        })?;
        assert!(
            crate::pipeline::run_one(&ConstantFold::new(), &mut fg, &crate::OptCtx::new(None))?
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
        Case { case: "fold_f32_neg_const", input: 2.0f32.to_bits() as u64, op: FloatUnaryOp::Neg, ty: ValueType::float(32), expected: (-2.0f32).to_bits() as u64 },
        Case { case: "fold_f64_abs_const", input: (-3.0f64).to_bits(), op: FloatUnaryOp::Abs, ty: ValueType::float(64), expected: 3.0f64.to_bits() },
        Case { case: "fold_f64_sqrt_const", input: 4.0f64.to_bits(), op: FloatUnaryOp::Sqrt, ty: ValueType::float(64), expected: 2.0f64.to_bits() },
    ];
    for c in &cases {
        let mut fg = make_fn(|b| {
            let v = b.build_float_const(c.input, c.ty);
            b.build_float_unary_op(v, c.op, c.ty)
        })?;
        assert!(
            crate::pipeline::run_one(&ConstantFold::new(), &mut fg, &crate::OptCtx::new(None))?
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
fn fold_f64_round_is_ties_away_from_zero() -> Result<()> {
    let cases: &[(f64, f64)] = &[
        (0.5, 1.0),
        (-0.5, -1.0),
        (1.5, 2.0),
        (-1.5, -2.0),
        (2.5, 3.0),
        (-2.5, -3.0),
        (3.5, 4.0),
        (-3.5, -4.0),
    ];
    for &(input, expected) in cases {
        let mut fg = make_fn(|b| {
            let v = b.build_float_const(input.to_bits(), ValueType::float(64));
            b.build_float_unary_op(v, FloatUnaryOp::Round, ValueType::float(64))
        })?;
        assert!(
            crate::pipeline::run_one(&ConstantFold::new(), &mut fg, &crate::OptCtx::new(None))?
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
fn fold_f32_round_is_ties_away_from_zero() -> Result<()> {
    let cases: &[(f32, f32)] = &[
        (0.5, 1.0),
        (-0.5, -1.0),
        (1.5, 2.0),
        (-1.5, -2.0),
        (2.5, 3.0),
        (-2.5, -3.0),
        (3.5, 4.0),
        (-3.5, -4.0),
    ];
    for &(input, expected) in cases {
        let mut fg = make_fn(|b| {
            let v = b.build_float_const(input.to_bits() as u64, ValueType::float(32));
            b.build_float_unary_op(v, FloatUnaryOp::Round, ValueType::float(32))
        })?;
        assert!(
            crate::pipeline::run_one(&ConstantFold::new(), &mut fg, &crate::OptCtx::new(None))?
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
    let mut fg = make_fn(|b| {
        let a = b.build_float_const(1.0f64.to_bits(), ValueType::float(64));
        let b2 = b.build_float_const(2.0f64.to_bits(), ValueType::float(64));
        let sum = b.build_float_binary_op(a, b2, FloatBinaryOp::Add, ValueType::float(64))?;
        let as_int = b.build_float_bits_to_int(sum, ValueType::int(64))?;
        let back_to_float = b.build_int_bits_to_float(as_int, ValueType::float(64))?;
        Ok(back_to_float)
    })?;
    assert!(
        crate::pipeline::run_one(&ConstantFold::new(), &mut fg, &crate::OptCtx::new(None))?
            .changed()
    );
    assert_return_kind(fg.graph(), NodeKind::FloatConst(3.0f64.to_bits()));
    Ok(())
}

/// A NaN's bit pattern is target-dependent, so `NaN + 1.0` stays a `FloatAdd`
/// rather than folding to a host-specific NaN constant.
#[test]
fn fold_f64_nan_plus_one_is_not_folded() -> Result<()> {
    let nan = f64::NAN.to_bits();
    let mut fg = make_fn(|b| {
        let a = b.build_float_const(nan, ValueType::float(64));
        let one = b.build_float_const(1.0f64.to_bits(), ValueType::float(64));
        b.build_float_binary_op(a, one, FloatBinaryOp::Add, ValueType::float(64))
    })?;
    assert!(
        !crate::pipeline::run_one(&ConstantFold::new(), &mut fg, &crate::OptCtx::new(None))?
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

/// The inner `Neg(inf)` still folds (a sign-bit flip, not a NaN), so the pass
/// reports a change, but the NaN-producing `Add` must survive.
#[test]
fn fold_f64_inf_minus_inf_add_is_not_folded() -> Result<()> {
    // `inf - inf` lowered to `Add(inf, Neg(inf))`.
    let inf = f64::INFINITY.to_bits();
    let mut fg = make_fn(|b| {
        let a = b.build_float_const(inf, ValueType::float(64));
        let bb = b.build_float_const(inf, ValueType::float(64));
        let neg_b = b.build_float_unary_op(bb, FloatUnaryOp::Neg, ValueType::float(64))?;
        b.build_float_binary_op(a, neg_b, FloatBinaryOp::Add, ValueType::float(64))
    })?;
    assert!(
        crate::pipeline::run_one(&ConstantFold::new(), &mut fg, &crate::OptCtx::new(None))?
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

/// A bitcast of a constant only a fold exposes folds as well: the builder
/// folds one only over a constant it is built on. 1 + 0x3f7fffff is 1.0f,
/// and 1.0f + 1.0f is 2.0f, 0x40000000.
/// Mutation: drop the two bitcast-of-constant rules.
#[test]
fn a_bitcast_of_a_folded_constant_folds() -> Result<()> {
    let mut fg = make_fn(|b| {
        let one = b.build_int_const(1u64, ValueType::int(32))?;
        let k = b.build_int_const(0x3f7f_ffffu64, ValueType::int(32))?;
        let sum = b.build_int_binary_operation(one, k, IntBinaryOp::Add, ValueType::int(32))?;
        b.build_int_bits_to_float(sum, ValueType::float(32))
    })?;
    crate::pipeline::run_one(&ConstantFold::new(), &mut fg, &crate::OptCtx::new(None))?;
    assert_return_kind(fg.graph(), NodeKind::FloatConst(0x3f80_0000));

    let mut fg = make_fn(|b| {
        let one = b.build_float_const(0x3f80_0000, ValueType::float(32));
        let sum = b.build_float_binary_op(one, one, FloatBinaryOp::Add, ValueType::float(32))?;
        b.build_float_bits_to_int(sum, ValueType::int(32))
    })?;
    crate::pipeline::run_one(&ConstantFold::new(), &mut fg, &crate::OptCtx::new(None))?;
    assert_returns_const(&fg, 0x4000_0000);
    Ok(())
}

/// `kind` over `input`, built raw so the builder's own fold stays out of it.
fn raw_bitcast(
    b: &mut strider_ir::FunctionBuilder,
    kind: NodeKind,
    input: strider_ir::node::ValueId,
    ty: ValueType,
) -> strider_ir::node::ValueId {
    use strider_ir::IRBuilder;
    use strider_ir::node::ValueKind;
    let node = b.create_node(kind, [input], [ValueKind::Typed(ty)]);
    let [out] = b.node_outputs_exact::<1>(node).expect("one output");
    out
}

/// A bitcast keeps every bit, both ways: each low byte of a 16-bit pattern
/// under high bytes spanning zero, normal, infinity / NaN and sign, and the
/// edges of the 32- and 64-bit ones. An 80-bit one has no `u64` float payload
/// and stays a bitcast.
#[test]
fn a_bitcast_of_a_constant_folds_to_the_same_bits() -> Result<()> {
    let edges = [0u64, 1, 0x7f, 0x80, 0x7fff_ffff, 0x8000_0000, 0xffff_ffff];
    let wide_edges = [0x7ff8_0000_0000_0001u64, 0x8000_0000_0000_0000, u64::MAX];
    let cases = [0x00u64, 0x3c, 0x7b, 0x7c, 0x7e, 0x80, 0xfc, 0xff]
        .into_iter()
        .flat_map(|hi| (0..=0xffu64).map(move |lo| (16, hi << 8 | lo)))
        .chain(edges.iter().map(|&k| (32, k)))
        .chain(edges.iter().chain(&wide_edges).map(|&k| (64, k)));
    for (bits, k) in cases {
        let (int_ty, float_ty) = (ValueType::int(bits), ValueType::float(bits));
        let mut fg = make_fn(|b| {
            let c = b.build_int_const(k, int_ty)?;
            Ok(raw_bitcast(b, NodeKind::IntBitsToFloat, c, float_ty))
        })?;
        crate::pipeline::run_one(&ConstantFold::new(), &mut fg, &crate::OptCtx::new(None))?;
        assert_return_kind(fg.graph(), NodeKind::FloatConst(k));

        let mut fg = make_fn(|b| {
            let c = b.build_float_const(k, float_ty);
            Ok(raw_bitcast(b, NodeKind::FloatBitsToInt, c, int_ty))
        })?;
        crate::pipeline::run_one(&ConstantFold::new(), &mut fg, &crate::OptCtx::new(None))?;
        assert_returns_const(&fg, k);
    }

    let mut fg = make_fn(|b| {
        let c = b.build_int_const(1u64, ValueType::int(80))?;
        Ok(raw_bitcast(
            b,
            NodeKind::IntBitsToFloat,
            c,
            ValueType::float(80),
        ))
    })?;
    crate::pipeline::run_one(&ConstantFold::new(), &mut fg, &crate::OptCtx::new(None))?;
    assert_return_kind(fg.graph(), NodeKind::IntBitsToFloat);
    Ok(())
}

/// Feeds the roundtrip a float Add rather than a constant, so the builder can't
/// fold the inner cast eagerly.
#[test]
fn fold_bitcast_roundtrip_f32() -> Result<()> {
    let mut fg = make_fn(|b| {
        let a = b.build_float_const(1.0f32.to_bits() as u64, ValueType::float(32));
        let b2 = b.build_float_const(1.5f32.to_bits() as u64, ValueType::float(32));
        let sum = b.build_float_binary_op(a, b2, FloatBinaryOp::Add, ValueType::float(32))?;
        let as_int = b.build_float_bits_to_int(sum, ValueType::int(32))?;
        b.build_int_bits_to_float(as_int, ValueType::float(32))
    })?;
    assert!(
        crate::pipeline::run_one(&ConstantFold::new(), &mut fg, &crate::OptCtx::new(None))?
            .changed()
    );
    assert_return_kind(fg.graph(), NodeKind::FloatConst(2.5f32.to_bits() as u64));
    Ok(())
}

/// Converging in ONE pass, without the outer pipeline loop, is what proves
/// consumers are re-enqueued onto the worklist after a rewrite.
#[test]
fn single_pass_propagates_through_chain() -> Result<()> {
    let mut fg = make_fn(|b| {
        let one = b.build_int_const(1u64, ValueType::int(32)).unwrap();
        let two = b.build_int_const(2u64, ValueType::int(32)).unwrap();
        let three = b.build_int_const(3u64, ValueType::int(32)).unwrap();
        let four = b.build_int_const(4u64, ValueType::int(32)).unwrap();
        let c1 = b.build_int_binary_operation(one, two, IntBinaryOp::Add, ValueType::int(32))?;
        let c2 = b.build_int_binary_operation(c1, three, IntBinaryOp::Add, ValueType::int(32))?;
        b.build_int_binary_operation(c2, four, IntBinaryOp::Add, ValueType::int(32))
    })?;

    crate::pipeline::run_one(&ConstantFold::new(), &mut fg, &crate::OptCtx::new(None))?;

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

/// A 10-deep sub chain, collapsed by the worklist re-enqueueing reassociation
/// rules along the way.
#[test]
fn fold_chain_of_ten_subs_reassociates() -> Result<()> {
    let vn = reg_vn(0x1000, 8);
    let (mut fg, x) = make_fn_with_var(vn, |b, x| {
        let mut acc = x;
        for _ in 0..10 {
            let one = b.build_int_const(1u64, ValueType::int(64)).unwrap();
            acc = b.build_sub_as_add_neg(acc, one, ValueType::int(64))?;
        }
        Ok(acc)
    })?;
    run_to_fixed_point(&ConstantFold::new(), &mut fg)?;
    assert_sub_with_const(&fg, x, 10, ValueType::int(64))?;
    Ok(())
}

// `Neg` is two's-complement negation; bitwise complement is the separate
// `Xor(x, all_ones)` shape. The MVN-based ARM lowering emits the Xor form, so
// conflating the two would fold `~49` to `-49`.

#[test]
fn fold_int_unary_not_is_two_complement_u32() -> Result<()> {
    let mut fg = make_fn(|b| {
        let c = b.build_int_const(50u64, ValueType::int(32)).unwrap();
        b.build_int_unary_operation(c, IntUnaryOp::Neg, ValueType::int(32))
    })?;
    assert!(
        crate::pipeline::run_one(&ConstantFold::new(), &mut fg, &crate::OptCtx::new(None))?
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

/// Two's complement of 0 is 0.
#[test]
fn fold_int_unary_not_zero_is_zero() -> Result<()> {
    let mut fg = make_fn(|b| {
        let c = b.build_int_const(0u64, ValueType::int(64)).unwrap();
        b.build_int_unary_operation(c, IntUnaryOp::Neg, ValueType::int(64))
    })?;
    assert!(
        crate::pipeline::run_one(&ConstantFold::new(), &mut fg, &crate::OptCtx::new(None))?
            .changed()
    );
    assert_returns_const(&fg, 0);
    Ok(())
}

/// An `I128` constant too wide for `u64`: the fold reaches its value through the
/// `int_const_u128` funnel.
#[test]
fn fold_i128_interner_backed_add_round_trip() -> Result<()> {
    let a: u128 = 1u128 << 100;
    let b_val: u128 = 1u128 << 101;
    let expected = a.wrapping_add(b_val);

    let mut fg = make_fn(|b| {
        let ca = b.build_int_const(a, ValueType::int(128))?;
        let cb = b.build_int_const(b_val, ValueType::int(128))?;
        b.build_int_binary_operation(ca, cb, IntBinaryOp::Add, ValueType::int(128))
    })?;

    let changed =
        crate::pipeline::run_one(&ConstantFold::new(), &mut fg, &crate::OptCtx::new(None))?
            .changed();
    assert!(
        changed,
        "ConstantFold must fold Add(I128, I128) to a constant"
    );

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

/// The all-ones arm reads `c` through `u128`, where an `I256` all-ones and an
/// `I256` `2^128` are indistinguishable.  Neither may take it.
#[test]
fn all_ones_rules_skip_past_128_bits() -> Result<()> {
    let wide_cases = [[0, 0, 1, 0], [u64::MAX; 4]];
    for op in [IntBinaryOp::And, IntBinaryOp::Or] {
        for limbs in wide_cases {
            let mut fg = make_fn(|b| {
                let addr = b.build_int_const(0xF00Du64, ValueType::int(64))?;
                let x = b.build_load(addr, rsleigh::VnSpace::RAM, ValueType::int(256))?;
                let c = b.build_int_const(UBig::from_words(&limbs), ValueType::int(256))?;
                b.build_int_binary_operation(x, c, op, ValueType::int(256))
            })?;
            let outcome =
                crate::pipeline::run_one(&ConstantFold::new(), &mut fg, &crate::OptCtx::new(None))?;
            assert!(
                !outcome.changed(),
                "{op:?} with the I256 constant {limbs:?} must not fold"
            );
            assert!(
                matches!(return_kind(fg.graph())?, NodeKind::IntBinaryOp(got) if got == op),
                "{op:?} node must remain"
            );
        }
    }
    Ok(())
}

/// The REASSOCIATION rules combine the two constants themselves rather than
/// through `eval::int_binary`, so its width guard does not cover them. At `I256`
/// both `2^127` operands fit in `u128` and `int_const_u128` hands them over, but
/// `wrapping_add` carries out at 128 bits: `2^127 + 2^127` becomes `0` instead
/// of `2^128`, and `(x + 2^127) + 2^127` folds to `x + 0`, i.e. to `x`.
#[test]
fn reassoc_add_skips_past_128_bits() -> Result<()> {
    let half: u128 = 1 << 127;
    let mut fg = make_fn(|b| {
        let addr = b.build_int_const(0xF00Du64, ValueType::int(64))?;
        let x = b.build_load(addr, rsleigh::VnSpace::RAM, ValueType::int(256))?;
        let c1 = b.build_int_const(half, ValueType::int(256))?;
        let inner = b.build_int_binary_operation(x, c1, IntBinaryOp::Add, ValueType::int(256))?;
        let c2 = b.build_int_const(half, ValueType::int(256))?;
        b.build_int_binary_operation(inner, c2, IntBinaryOp::Add, ValueType::int(256))
    })?;
    let outcome =
        crate::pipeline::run_one(&ConstantFold::new(), &mut fg, &crate::OptCtx::new(None))?;
    assert!(
        !outcome.changed(),
        "an I256 add-reassociation must not fold: the combined constant does not \
         fit the u128 the rule computes in"
    );
    Ok(())
}

/// The same hole on the `Mul` side: at `I256` both `2^64` operands fit in
/// `u128`, but `wrapping_mul` carries out at 128 bits, so `2^64 * 2^64` becomes
/// `0` instead of `2^128` and `(x * 2^64) * 2^64` would fold to `x * 0`. The
/// rule's own width guard and the template's refusal to intern a `u128` past
/// 128 bits each stop it; lifting both is what makes this fold.
#[test]
fn reassoc_mul_skips_past_128_bits() -> Result<()> {
    let two_64: u128 = 1 << 64;
    let mut fg = make_fn(|b| {
        let addr = b.build_int_const(0xF00Du64, ValueType::int(64))?;
        let x = b.build_load(addr, rsleigh::VnSpace::RAM, ValueType::int(256))?;
        let c1 = b.build_int_const(two_64, ValueType::int(256))?;
        let inner = b.build_int_binary_operation(x, c1, IntBinaryOp::Mul, ValueType::int(256))?;
        let c2 = b.build_int_const(two_64, ValueType::int(256))?;
        b.build_int_binary_operation(inner, c2, IntBinaryOp::Mul, ValueType::int(256))
    })?;
    let outcome =
        crate::pipeline::run_one(&ConstantFold::new(), &mut fg, &crate::OptCtx::new(None))?;
    assert!(
        !outcome.changed(),
        "an I256 mul-reassociation must not fold: the product does not fit the \
         u128 the rule computes in"
    );
    Ok(())
}

/// Int const-eval computes in `UBig` at the declared width: shifts, the
/// overflow arms and the signed compares all run past 128 bits.
#[test]
fn int_const_eval_folds_past_128_bits() -> Result<()> {
    let i256 = ValueType::int(256);

    // `1 << 130` at I256 is 2^130, not `1 << (130 - 128)`.
    let mut fg = make_fn(|b| {
        let l = b.build_int_const(1u64, i256)?;
        let r = b.build_int_const(130u64, i256)?;
        b.build_int_binary_operation(l, r, IntBinaryOp::ShiftLeft, i256)
    })?;
    assert!(
        crate::pipeline::run_one(&ConstantFold::new(), &mut fg, &crate::OptCtx::new(None))?
            .changed(),
        "I256 shift must fold"
    );
    assert_eq!(
        fg.int_const(return_value(fg.graph())?),
        Some(&(UBig::ONE << 130usize)),
        "1 << 130 at I256"
    );

    // `Carry(2^128 - 1, 1)` overflows 128 bits but not 256.
    let mut fg = make_fn(|b| {
        let l = b.build_int_const(u128::MAX, i256)?;
        let r = b.build_int_const(1u64, i256)?;
        b.build_int_cmp_operation(l, r, IntCmpOp::Carry, i256)
    })?;
    assert!(
        crate::pipeline::run_one(&ConstantFold::new(), &mut fg, &crate::OptCtx::new(None))?
            .changed(),
        "I256 Carry must fold"
    );
    assert_eq!(
        fg.int_const_u128(return_value(fg.graph())?),
        Some(0),
        "2^128 - 1 plus 1 does not carry out of 256 bits"
    );

    // A signed compare past the carrier reads the I256 sign bit.
    let mut fg = make_fn(|b| {
        let l = b.build_int_const(UBig::ONE << 255usize, i256)?;
        let r = b.build_int_const(2u64, i256)?;
        b.build_int_cmp_operation(l, r, IntCmpOp::Sless, i256)
    })?;
    assert!(
        crate::pipeline::run_one(&ConstantFold::new(), &mut fg, &crate::OptCtx::new(None))?
            .changed(),
        "I256 Sless must fold"
    );
    assert_eq!(
        fg.int_const_u128(return_value(fg.graph())?),
        Some(1),
        "2^255 at I256 is INT_MIN, below 2"
    );
    Ok(())
}

/// Signed division at I256: `INT_MIN / -1` is still undefined, and a wide
/// negative dividend divides as two's complement at 256 bits.
#[test]
fn int_const_eval_sdiv_past_128_bits() -> Result<()> {
    let i256 = ValueType::int(256);
    let min = UBig::ONE << 255usize;
    let all_ones = (UBig::ONE << 256usize) - UBig::ONE;

    // -(2^200) / 4 == -(2^198).
    let mut fg = make_fn(|b| {
        let l = b.build_int_const((UBig::ONE << 256usize) - (UBig::ONE << 200usize), i256)?;
        let r = b.build_int_const(4u64, i256)?;
        b.build_int_binary_operation(l, r, IntBinaryOp::Sdiv, i256)
    })?;
    assert!(
        crate::pipeline::run_one(&ConstantFold::new(), &mut fg, &crate::OptCtx::new(None))?
            .changed(),
        "I256 Sdiv must fold"
    );
    assert_eq!(
        fg.int_const(return_value(fg.graph())?),
        Some(&((UBig::ONE << 256usize) - (UBig::ONE << 198usize))),
        "-(2^200) / 4 at I256"
    );

    // INT_MIN / -1 has no I256 result.
    let mut fg = make_fn(|b| {
        let l = b.build_int_const(min, i256)?;
        let r = b.build_int_const(all_ones, i256)?;
        b.build_int_binary_operation(l, r, IntBinaryOp::Sdiv, i256)
    })?;
    let outcome =
        crate::pipeline::run_one(&ConstantFold::new(), &mut fg, &crate::OptCtx::new(None))?;
    assert!(!outcome.changed(), "I256 INT_MIN / -1 has no value");
    Ok(())
}

/// `SignExtend` from I256 to I512 copies the I256 sign bit across the top
/// 256 bits.
#[test]
fn int_const_eval_sign_extend_to_512_bits() -> Result<()> {
    let (i256, i512) = (ValueType::int(256), ValueType::int(512));
    let mut fg = make_fn(|b| {
        // -(2^130) at I256.
        let v = b.build_int_const((UBig::ONE << 256usize) - (UBig::ONE << 130usize), i256)?;
        b.extend_if_needed(v, i512, strider_ir::ExtendOp::SignExtend)
    })?;
    assert!(
        crate::pipeline::run_one(&ConstantFold::new(), &mut fg, &crate::OptCtx::new(None))?
            .changed(),
        "I256 -> I512 sign extend must fold"
    );
    assert_eq!(
        fg.int_const(return_value(fg.graph())?),
        Some(&((UBig::ONE << 512usize) - (UBig::ONE << 130usize))),
        "-(2^130) at I512"
    );
    Ok(())
}

/// The unique `Mul` the factoring rules produce: base and coefficient.
fn assert_mul_with_const(
    fg: &strider_ir::Function,
    expected_base: strider_ir::node::ValueId,
    expected_const: u128,
    ty: ValueType,
) -> Result<()> {
    let val = return_value(fg.graph())?;
    let node = fg.producer(val);
    assert!(
        matches!(fg.node_kind(node), NodeKind::IntBinaryOp(IntBinaryOp::Mul)),
        "expected a Mul, got {:?}",
        fg.node_kind(node)
    );
    let inputs = fg.node_inputs(node);
    assert_eq!(inputs.len(), 2);
    let (base, k) = if inputs[0] == expected_base {
        (inputs[0], inputs[1])
    } else {
        (inputs[1], inputs[0])
    };
    assert_eq!(base, expected_base, "Mul must keep the shared operand");
    assert_eq!(fg.int_const_u128(k), Some(expected_const));
    assert_eq!(fg.value_type(k)?, ty);
    Ok(())
}

/// x + x*2 -> x*3
#[test]
fn factor_add_self_mul() -> Result<()> {
    let (mut fg, x) = make_fn_with_var(reg_vn(0x1000, 8), |b, x| {
        let two = b.build_int_const(2u64, ValueType::int(64)).unwrap();
        let m = b.build_int_binary_operation(x, two, IntBinaryOp::Mul, ValueType::int(64))?;
        b.build_int_binary_operation(x, m, IntBinaryOp::Add, ValueType::int(64))
    })?;
    run_to_fixed_point(&ConstantFold::new(), &mut fg)?;
    assert_mul_with_const(&fg, x, 3, ValueType::int(64))
}

/// x + x -> x*2: both sides take the alternation's bare arm.
#[test]
fn factor_add_self_self() -> Result<()> {
    let (mut fg, x) = make_fn_with_var(reg_vn(0x1000, 8), |b, x| {
        b.build_int_binary_operation(x, x, IntBinaryOp::Add, ValueType::int(64))
    })?;
    run_to_fixed_point(&ConstantFold::new(), &mut fg)?;
    assert_mul_with_const(&fg, x, 2, ValueType::int(64))
}

/// x*3 + x*3 -> x*6. The bare arm would also match, binding `x` to the whole
/// product and leaving a `Mul` behind; the product arm is listed first so the
/// coefficient is collected instead. Reordering the arms still reaches x*6,
/// because the mul reassociation collapses the `(x*3)*2` that fallthrough
/// would build.
#[test]
fn factor_add_equal_products_collects_the_coefficient() -> Result<()> {
    let (mut fg, x) = make_fn_with_var(reg_vn(0x1000, 8), |b, x| {
        let c3 = b.build_int_const(3u64, ValueType::int(64)).unwrap();
        let m = b.build_int_binary_operation(x, c3, IntBinaryOp::Mul, ValueType::int(64))?;
        b.build_int_binary_operation(m, m, IntBinaryOp::Add, ValueType::int(64))
    })?;
    run_to_fixed_point(&ConstantFold::new(), &mut fg)?;
    assert_mul_with_const(&fg, x, 6, ValueType::int(64))
}

/// x - x -> 0, which the identity rule reaches before the difference rule
/// would build an `x*0`.
#[test]
fn factor_sub_self_self_is_zero() -> Result<()> {
    let (mut fg, _x) = make_fn_with_var(reg_vn(0x1000, 8), |b, x| {
        b.build_sub_as_add_neg(x, x, ValueType::int(64))
    })?;
    run_to_fixed_point(&ConstantFold::new(), &mut fg)?;
    let val = return_value(fg.graph())?;
    assert_eq!(fg.int_const_u128(val), Some(0));
    Ok(())
}

/// x*3 + x*5 -> x*8
#[test]
fn factor_add_mul_mul() -> Result<()> {
    let (mut fg, x) = make_fn_with_var(reg_vn(0x1000, 8), |b, x| {
        let c3 = b.build_int_const(3u64, ValueType::int(64)).unwrap();
        let c5 = b.build_int_const(5u64, ValueType::int(64)).unwrap();
        let a = b.build_int_binary_operation(x, c3, IntBinaryOp::Mul, ValueType::int(64))?;
        let c = b.build_int_binary_operation(x, c5, IntBinaryOp::Mul, ValueType::int(64))?;
        b.build_int_binary_operation(a, c, IntBinaryOp::Add, ValueType::int(64))
    })?;
    run_to_fixed_point(&ConstantFold::new(), &mut fg)?;
    assert_mul_with_const(&fg, x, 8, ValueType::int(64))
}

/// x + (x << 3) -> x*9
#[test]
fn factor_add_self_shl() -> Result<()> {
    let (mut fg, x) = make_fn_with_var(reg_vn(0x1000, 8), |b, x| {
        let three = b.build_int_const(3u64, ValueType::int(64)).unwrap();
        let s =
            b.build_int_binary_operation(x, three, IntBinaryOp::ShiftLeft, ValueType::int(64))?;
        b.build_int_binary_operation(x, s, IntBinaryOp::Add, ValueType::int(64))
    })?;
    run_to_fixed_point(&ConstantFold::new(), &mut fg)?;
    assert_mul_with_const(&fg, x, 9, ValueType::int(64))
}

/// p-code reads a shift count unsigned, so a boolean count of 1 shifts by 1:
/// x + (x << 1:i1) -> x*3.
#[test]
fn factor_reads_a_boolean_shift_count_unsigned() -> Result<()> {
    use strider_ir::IRBuilder;
    use strider_ir::node::ValueKind;
    let (mut fg, x) = make_fn_with_var(reg_vn(0x1000, 8), |b, x| {
        let one = b.build_int_const(1u64, ValueType::I1)?;
        let shl = b.create_node(
            NodeKind::IntBinaryOp(IntBinaryOp::ShiftLeft),
            [x, one],
            [ValueKind::Typed(ValueType::int(64))],
        );
        let [s] = b.node_outputs_exact::<1>(shl)?;
        b.build_int_binary_operation(x, s, IntBinaryOp::Add, ValueType::int(64))
    })?;
    run_to_fixed_point(&ConstantFold::new(), &mut fg)?;
    assert_mul_with_const(&fg, x, 3, ValueType::int(64))
}

/// A count typed past 128 bits still reads its value: x + (x << 2:i256) -> x*5.
#[test]
fn factor_reads_a_shift_count_wider_than_128_bits() -> Result<()> {
    use strider_ir::IRBuilder;
    use strider_ir::node::ValueKind;
    let (mut fg, x) = make_fn_with_var(reg_vn(0x1000, 8), |b, x| {
        let two = b.build_int_const(2u64, ValueType::int(256))?;
        let shl = b.create_node(
            NodeKind::IntBinaryOp(IntBinaryOp::ShiftLeft),
            [x, two],
            [ValueKind::Typed(ValueType::int(64))],
        );
        let [s] = b.node_outputs_exact::<1>(shl)?;
        b.build_int_binary_operation(x, s, IntBinaryOp::Add, ValueType::int(64))
    })?;
    run_to_fixed_point(&ConstantFold::new(), &mut fg)?;
    assert_mul_with_const(&fg, x, 5, ValueType::int(64))
}

/// x - x*3 -> x * (1 - 3), wrapping at the type width.
#[test]
fn factor_sub_self_mul_wraps() -> Result<()> {
    let (mut fg, x) = make_fn_with_var(reg_vn(0x1000, 8), |b, x| {
        let c3 = b.build_int_const(3u64, ValueType::int(64)).unwrap();
        let m = b.build_int_binary_operation(x, c3, IntBinaryOp::Mul, ValueType::int(64))?;
        b.build_sub_as_add_neg(x, m, ValueType::int(64))
    })?;
    run_to_fixed_point(&ConstantFold::new(), &mut fg)?;
    assert_mul_with_const(&fg, x, u128::from(u64::MAX) - 1, ValueType::int(64))
}

/// x*9 - x*4 -> x*5
#[test]
fn factor_sub_mul_mul() -> Result<()> {
    let (mut fg, x) = make_fn_with_var(reg_vn(0x1000, 8), |b, x| {
        let c9 = b.build_int_const(9u64, ValueType::int(64)).unwrap();
        let c4 = b.build_int_const(4u64, ValueType::int(64)).unwrap();
        let a = b.build_int_binary_operation(x, c9, IntBinaryOp::Mul, ValueType::int(64))?;
        let c = b.build_int_binary_operation(x, c4, IntBinaryOp::Mul, ValueType::int(64))?;
        b.build_sub_as_add_neg(a, c, ValueType::int(64))
    })?;
    run_to_fixed_point(&ConstantFold::new(), &mut fg)?;
    assert_mul_with_const(&fg, x, 5, ValueType::int(64))
}

/// A shift count is an ordinary constant of the shifted type, so it can name a
/// value no `u128` can shift by. The rule must decline rather than evaluate
/// `1 << 200` and panic the whole lift.
#[test]
fn factor_declines_an_out_of_range_shift() -> Result<()> {
    let (mut fg, _x) = make_fn_with_var(reg_vn(0x1000, 8), |b, x| {
        let wide = b.build_int_const(200u64, ValueType::int(64)).unwrap();
        let s =
            b.build_int_binary_operation(x, wide, IntBinaryOp::ShiftLeft, ValueType::int(64))?;
        b.build_int_binary_operation(x, s, IntBinaryOp::Add, ValueType::int(64))
    })?;
    run_to_fixed_point(&ConstantFold::new(), &mut fg)?;
    let val = return_value(fg.graph())?;
    assert!(
        !matches!(
            fg.node_kind(fg.producer(val)),
            NodeKind::IntBinaryOp(IntBinaryOp::Mul)
        ),
        "a width-or-wider shift count must not fold to a multiply"
    );
    Ok(())
}

/// A count typed past 128 bits is still out of range when its value is: 64
/// at an `I64`'s width, `2^128` past what any `u128` holds. The shift stays.
#[test]
fn factor_declines_a_wide_count_out_of_range() -> Result<()> {
    use strider_ir::IRBuilder;
    use strider_ir::node::ValueKind;
    for limbs in [[64, 0, 0, 0], [0, 0, 1, 0]] {
        let (mut fg, _x) = make_fn_with_var(reg_vn(0x1000, 8), |b, x| {
            let count = b.build_int_const(UBig::from_words(&limbs), ValueType::int(256))?;
            let shl = b.create_node(
                NodeKind::IntBinaryOp(IntBinaryOp::ShiftLeft),
                [x, count],
                [ValueKind::Typed(ValueType::int(64))],
            );
            let [s] = b.node_outputs_exact::<1>(shl)?;
            b.build_int_binary_operation(x, s, IntBinaryOp::Add, ValueType::int(64))
        })?;
        run_to_fixed_point(&ConstantFold::new(), &mut fg)?;
        let val = return_value(fg.graph())?;
        let add = fg.producer(val);
        assert!(
            matches!(fg.node_kind(add), NodeKind::IntBinaryOp(IntBinaryOp::Add))
                && fg.node_inputs(add).into_iter().any(|v| matches!(
                    fg.node_kind(fg.producer(v)),
                    NodeKind::IntBinaryOp(IntBinaryOp::ShiftLeft)
                )),
            "count {limbs:?}: x + (x << c) must stay unfolded, got {:?}",
            fg.node_kind(add)
        );
    }
    Ok(())
}

/// Coefficient of `base` in a value built only from `base`, integer constants,
/// `Add` / `Mul` / `Neg` / `ShiftLeft`, evaluated in `u128` and left unmasked.
fn linear_coeff(
    fg: &strider_ir::Function,
    val: strider_ir::node::ValueId,
    base: strider_ir::node::ValueId,
) -> Option<u128> {
    if val == base {
        return Some(1);
    }
    let node = fg.producer(val);
    let ins = fg.node_inputs(node);
    match fg.node_kind(node) {
        NodeKind::IntConst(_) => (fg.int_const_u128(val) == Some(0)).then_some(0),
        NodeKind::IntUnaryOp(IntUnaryOp::Neg) => {
            Some(0u128.wrapping_sub(linear_coeff(fg, ins[0], base)?))
        }
        NodeKind::IntBinaryOp(IntBinaryOp::Add) => {
            Some(linear_coeff(fg, ins[0], base)?.wrapping_add(linear_coeff(fg, ins[1], base)?))
        }
        NodeKind::IntBinaryOp(IntBinaryOp::Mul) => {
            let (k, other) = match fg.int_const_u128(ins[0]) {
                Some(k) => (k, ins[1]),
                None => (fg.int_const_u128(ins[1])?, ins[0]),
            };
            Some(linear_coeff(fg, other, base)?.wrapping_mul(k))
        }
        NodeKind::IntBinaryOp(IntBinaryOp::ShiftLeft) => {
            let n = fg.int_const_u128(ins[1])?;
            let c = linear_coeff(fg, ins[0], base)?;
            Some(if n >= 128 { 0 } else { c << n })
        }
        _ => None,
    }
}

/// `(x * C1) * C2` combines its coefficients with `wrapping_mul`, i.e. modulo
/// `2^128`. Brute-forced at every width the carrier holds: the width's modulus
/// divides `2^128`, so the result must equal the product computed in that
/// width directly.
#[test]
fn reassoc_mul_is_exact_at_every_carrier_width() -> Result<()> {
    let widths = [
        ValueType::I1,
        ValueType::int(8),
        ValueType::int(16),
        ValueType::int(24),
        ValueType::int(32),
        ValueType::int(64),
        ValueType::int(80),
        ValueType::int(128),
    ];
    for ty in widths {
        let bits = ty.bit_width();
        let mask = width_mask(bits);
        for &c1 in &edge_coefficients(bits) {
            for &c2 in &edge_coefficients(bits) {
                let (mut fg, base) = nested_mul_shape(ty, c1, c2)?;
                run_to_fixed_point(&ConstantFold::new(), &mut fg)?;
                let got = linear_coeff(&fg, return_value(fg.graph())?, base)
                    .map(|k| k & mask)
                    .ok_or_else(|| {
                        anyhow!("{ty:?} with ({c1:#x}, {c2:#x}) left a shape that is no multiple of the base")
                    })?;
                assert_eq!(got, c1.wrapping_mul(c2) & mask, "{ty:?} ({c1:#x}, {c2:#x})");
            }
        }
    }
    Ok(())
}

/// `(base * c1) * c2` over a base of type `ty`, returning the function and the
/// base value. At `I1` the base is a comparison, as in [`factor_shape`].
fn nested_mul_shape(
    ty: ValueType,
    c1: u128,
    c2: u128,
) -> Result<(strider_ir::Function, strider_ir::node::ValueId)> {
    let byte_size = u32::try_from(if ty == ValueType::I1 {
        8
    } else {
        ty.byte_size()
    })?;
    let mut captured = None;
    let (fg, x) = make_fn_with_var(reg_vn(0x1000, byte_size), |b, x| {
        let base = if ty == ValueType::I1 {
            let zero = b.build_int_const(0u64, ValueType::int(64))?;
            let cmp = b.build_int_cmp_operation(x, zero, IntCmpOp::Equal, ValueType::int(64))?;
            captured = Some(cmp);
            cmp
        } else {
            x
        };
        let k1 = b.build_int_const(c1, ty)?;
        let inner = b.build_int_binary_operation(base, k1, IntBinaryOp::Mul, ty)?;
        let k2 = b.build_int_const(c2, ty)?;
        b.build_int_binary_operation(inner, k2, IntBinaryOp::Mul, ty)
    })?;
    Ok((fg, captured.unwrap_or(x)))
}

/// The four non-shift factoring forms, as `(c1, c2)` pairs of coefficients on
/// the shared operand.
#[derive(Clone, Copy, Debug)]
enum FactorForm {
    AddSelf,
    AddMul,
    SubSelf,
    SubMul,
}

/// One side of a factoring shape: which arm of the rule's alternation it
/// takes.
#[derive(Clone, Copy, Debug)]
enum FactorArm {
    Bare,
    Mul(u128),
    Shl(u128),
}

impl FactorArm {
    /// What the arm multiplies the base by, in `u128`: the rule's own
    /// `coefficient`, independently written.
    fn coefficient(self) -> u128 {
        match self {
            FactorArm::Bare => 1,
            FactorArm::Mul(c) => c,
            FactorArm::Shl(z) => 1u128 << z,
        }
    }
}

/// Builds one factoring shape over a base of type `ty`, returning the function
/// and the base value. At `I1` the base is a comparison, the only non-constant
/// one-bit value a test can produce.
fn factor_shape(
    form: FactorForm,
    ty: ValueType,
    c1: u128,
    c2: u128,
) -> Result<(strider_ir::Function, strider_ir::node::ValueId)> {
    let (left, right, subtract) = match form {
        FactorForm::AddSelf => (FactorArm::Bare, FactorArm::Mul(c1), false),
        FactorForm::SubSelf => (FactorArm::Bare, FactorArm::Mul(c1), true),
        FactorForm::AddMul => (FactorArm::Mul(c1), FactorArm::Mul(c2), false),
        FactorForm::SubMul => (FactorArm::Mul(c1), FactorArm::Mul(c2), true),
    };
    factor_arms_shape(ty, left, right, subtract)
}

/// `left(base) + right(base)`, or their difference, over a base of type `ty`.
fn factor_arms_shape(
    ty: ValueType,
    left: FactorArm,
    right: FactorArm,
    subtract: bool,
) -> Result<(strider_ir::Function, strider_ir::node::ValueId)> {
    let byte_size = u32::try_from(if ty == ValueType::I1 {
        8
    } else {
        ty.byte_size()
    })?;
    let mut captured = None;
    let (fg, x) = make_fn_with_var(reg_vn(0x1000, byte_size), |b, x| {
        let base = if ty == ValueType::I1 {
            let zero = b.build_int_const(0u64, ValueType::int(64))?;
            let cmp = b.build_int_cmp_operation(x, zero, IntCmpOp::Equal, ValueType::int(64))?;
            captured = Some(cmp);
            cmp
        } else {
            x
        };
        let mut arm = |a: FactorArm| -> Result<strider_ir::node::ValueId> {
            let (k, op) = match a {
                FactorArm::Bare => return Ok(base),
                FactorArm::Mul(c) => (c, IntBinaryOp::Mul),
                FactorArm::Shl(z) => (z, IntBinaryOp::ShiftLeft),
            };
            let k = b.build_int_const(k, ty)?;
            b.build_int_binary_operation(base, k, op, ty)
        };
        let (l, r) = (arm(left)?, arm(right)?);
        if subtract {
            b.build_sub_as_add_neg(l, r, ty)
        } else {
            b.build_int_binary_operation(l, r, IntBinaryOp::Add, ty)
        }
    })?;
    Ok((fg, captured.unwrap_or(x)))
}

/// Every ordered pairing of the rule's three arms, summed and differenced, at
/// four widths. The shift arm's coefficient is `1 << Z`, the one that grows
/// with the root's width, so each width also runs its own largest in-range
/// count.
#[test]
fn factor_arm_pairs_fold_at_every_width() -> Result<()> {
    for ty in [
        ValueType::int(8),
        ValueType::int(16),
        ValueType::int(32),
        ValueType::int(128),
    ] {
        let bits = ty.bit_width();
        let mask = width_mask(bits);
        let arms = [
            FactorArm::Bare,
            FactorArm::Mul(3),
            FactorArm::Mul(mask),
            FactorArm::Shl(1),
            FactorArm::Shl(bits as u128 - 1),
        ];
        for left in arms {
            for right in arms {
                for subtract in [false, true] {
                    let (l, r) = (left.coefficient(), right.coefficient());
                    let want = if subtract {
                        l.wrapping_sub(r)
                    } else {
                        l.wrapping_add(r)
                    } & mask;
                    let (mut fg, base) = factor_arms_shape(ty, left, right, subtract)?;
                    run_to_fixed_point(&ConstantFold::new(), &mut fg)?;
                    let val = return_value(fg.graph())?;
                    let root = fg.producer(val);
                    let what = format!(
                        "{left:?} {} {right:?} at {ty:?}",
                        if subtract { '-' } else { '+' }
                    );
                    // Every unfolded shape roots at the `Add` the rule
                    // matches, so this is what says the rule fired.
                    assert!(
                        !matches!(fg.node_kind(root), NodeKind::IntBinaryOp(IntBinaryOp::Add)),
                        "{what} did not collapse: {:?}",
                        fg.node_kind(root)
                    );
                    let got = linear_coeff(&fg, val, base)
                        .map(|k| k & mask)
                        .ok_or_else(|| {
                            anyhow!("{what} left a shape that is no multiple of the base")
                        })?;
                    assert_eq!(got, want, "{what}");
                }
            }
        }
    }
    Ok(())
}

/// The all-ones value of a `bits`-wide integer.
fn width_mask(bits: usize) -> u128 {
    if bits >= 128 {
        u128::MAX
    } else {
        (1u128 << bits) - 1
    }
}

/// Edge coefficients at `bits`: 0, 1, 2, the width's max and max-1, and its
/// sign bit.
fn edge_coefficients(bits: usize) -> Vec<u128> {
    let mask = width_mask(bits);
    let mut cs = vec![0, 1, 2, mask, mask.wrapping_sub(1), 1u128 << (bits - 1)];
    cs.iter_mut().for_each(|c| *c &= mask);
    cs.sort_unstable();
    cs.dedup();
    cs
}

/// The four non-shift factoring rules compute their coefficient in `u128`.
/// Brute-forced at every width that carrier holds: whatever chain of rules
/// fires, the folded value must stay the same multiple of the base it started
/// as, computed independently in the width's modulus.
#[test]
fn factor_coefficients_are_exact_at_every_carrier_width() -> Result<()> {
    use FactorForm::{AddMul, AddSelf, SubMul, SubSelf};
    let widths = [
        ValueType::I1,
        ValueType::int(8),
        ValueType::int(16),
        ValueType::int(24),
        ValueType::int(32),
        ValueType::int(64),
        ValueType::int(80),
        ValueType::int(128),
    ];
    for ty in widths {
        let bits = ty.bit_width();
        let mask = width_mask(bits);
        for &c1 in &edge_coefficients(bits) {
            for &c2 in &edge_coefficients(bits) {
                for form in [AddSelf, AddMul, SubSelf, SubMul] {
                    let expected = match form {
                        AddSelf => c1.wrapping_add(1),
                        AddMul => c1.wrapping_add(c2),
                        SubSelf => 1u128.wrapping_sub(c1),
                        SubMul => c1.wrapping_sub(c2),
                    } & mask;
                    let (mut fg, base) = factor_shape(form, ty, c1, c2)?;
                    run_to_fixed_point(&ConstantFold::new(), &mut fg)?;
                    let got = linear_coeff(&fg, return_value(fg.graph())?, base)
                        .map(|k| k & mask)
                        .ok_or_else(|| {
                            anyhow!("{form:?} at {ty:?} with ({c1:#x}, {c2:#x}) left a shape that is no multiple of the base")
                        })?;
                    assert_eq!(got, expected, "{form:?} at {ty:?} with ({c1:#x}, {c2:#x})");
                }
            }
        }
    }
    Ok(())
}

/// Past the `u128` carrier `get_uint` still answers `Some` for a value that
/// fits it, so without a width bound of their own the four non-shift forms
/// would compute the coefficient in the wrong modulus. They must decline.
#[test]
fn factor_declines_past_the_u128_carrier() -> Result<()> {
    use FactorForm::{AddMul, AddSelf, SubMul, SubSelf};
    for ty in [ValueType::int(256), ValueType::int(512)] {
        for form in [AddSelf, AddMul, SubSelf, SubMul] {
            // 2^128 - 1 is the coefficient that exposes the wrong modulus:
            // `C + 1` is 2^128 at these widths and 0 in the carrier.
            let (mut fg, _base) = factor_shape(form, ty, u128::MAX, 3)?;
            run_to_fixed_point(&ConstantFold::new(), &mut fg)?;
            let root = fg.producer(return_value(fg.graph())?);
            assert!(
                matches!(fg.node_kind(root), NodeKind::IntBinaryOp(IntBinaryOp::Add)),
                "{form:?} at {ty:?} folded to {:?}; the coefficient does not fit the u128 the rule computes in",
                fg.node_kind(root)
            );
        }
    }
    Ok(())
}

/// `x - 0` and `x + 0` never overflow, whichever side `Scarry`'s `0` sits on;
/// `0 - x` overflows at `x == INT_MIN`, so `Sborrow(0, x)` stays.
#[test]
fn overflow_flag_against_zero_folds_to_false() -> Result<()> {
    let vn = reg_vn(0x1000, 4);
    for (op, zero_left, folds) in [
        (IntCmpOp::Sborrow, false, true),
        (IntCmpOp::Scarry, false, true),
        (IntCmpOp::Scarry, true, true),
        (IntCmpOp::Sborrow, true, false),
    ] {
        let (mut fg, _x) = make_fn_with_var(vn, |b, x| {
            let zero = b.build_int_const(0u64, ValueType::int(32))?;
            let (l, r) = if zero_left { (zero, x) } else { (x, zero) };
            b.build_int_cmp_operation(l, r, op, ValueType::int(32))
        })?;
        run_to_fixed_point(&ConstantFold::new(), &mut fg)?;
        let ret = return_value(fg.graph())?;
        if folds {
            assert_eq!(
                (fg.int_const_u128(ret), fg.value_type_opt(ret)),
                (Some(0), Some(ValueType::I1)),
                "{op:?} zero_left={zero_left} must fold to false"
            );
        } else {
            assert_eq!(
                *fg.node_kind(fg.producer(ret)),
                NodeKind::IntCmpOp(op),
                "{op:?} zero_left={zero_left} must stay"
            );
        }
    }
    Ok(())
}

/// A `Capture` draws its id from a process-wide counter, so a capture minted
/// inside a rule body renumbers every later rule's captures. Keeping the pool
/// at the top of each builder is what makes reordering the rules within a
/// builder a no-op.
#[test]
fn every_rule_capture_is_minted_before_the_first_rule() {
    let src = include_str!("rules.rs");
    let mut builders = 0;
    for chunk in src.split("\nfn ").skip(1) {
        let Some(body) = chunk.strip_prefix("build_") else {
            continue;
        };
        let name = body.split('(').next().unwrap_or_default();
        let Some(first_rule) = body.find("rewrite_rule(") else {
            continue;
        };
        builders += 1;
        let minted: Vec<usize> = body
            .match_indices("Capture::new()")
            .map(|(at, _)| at)
            .collect();
        assert!(
            !minted.is_empty(),
            "build_{name} declares rules but mints no capture"
        );
        for at in minted {
            assert!(
                at < first_rule,
                "build_{name} mints a capture inside a rule body"
            );
        }
    }
    assert_eq!(builders, 5, "the rule builders that mint captures");
    // The check above reads one spelling, so a pool reaching a builder by any
    // other route would pass it unseen.
    assert!(
        !src.contains("-> Capture") && !src.contains("Capture::internal"),
        "every capture here is minted by the literal the check reads"
    );
}

/// A coefficient rule computes in `u128`, so every one of them is gated on
/// the root having a `u128` modulus: the multiply form reads the gate off
/// the root type, the shift form off `shift_count_in_range`. Past the
/// carrier `C + 1` and `1 << C` would both be computed in the wrong ring, so
/// the rules must decline rather than fold.
#[test]
fn a_coefficient_rule_declines_past_the_carrier() -> Result<()> {
    for (width, folds) in [(64u16, true), (256, false)] {
        let ty = ValueType::int(width);
        let vn = reg_vn(0x2000, u32::from(width) / 8);
        for op in [IntBinaryOp::Mul, IntBinaryOp::ShiftLeft] {
            let (mut fg, _x) = make_fn_with_var(vn, |b, x| {
                let three = b.build_int_const(3u64, ty)?;
                let scaled = b.build_int_binary_operation(x, three, op, ty)?;
                b.build_int_binary_operation(x, scaled, IntBinaryOp::Add, ty)
            })?;
            run_to_fixed_point(&ConstantFold::new(), &mut fg)?;
            let want = if folds {
                IntBinaryOp::Mul
            } else {
                IntBinaryOp::Add
            };
            assert_eq!(
                return_kind(fg.graph())?,
                NodeKind::IntBinaryOp(want),
                "x + (x {op:?} 3) at i{width}"
            );
        }
    }
    Ok(())
}
