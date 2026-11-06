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
    run_pass, run_to_fixed_point,
};
use strider_ir_test_utils::{RegisterSet, reg_vn};

/// The operand constants die in the fold, so the fresh `IntConst(7)` must
/// absorb their asm-fingerprints or their addresses are lost.
#[test]
fn const_eval_absorbs_operand_fingerprints() -> Result<()> {
    const A: u64 = 0xC0FF_EE01;
    const B: u64 = 0xC0FF_EE02;
    const ADD: u64 = 0xC0FF_EE03;
    let mut fg = make_fn(|b| {
        b.set_lift_addr(Some(A));
        let c3 = b.build_int_const(3u64, ValueType::int(64))?;
        b.set_lift_addr(Some(B));
        let c4 = b.build_int_const(4u64, ValueType::int(64))?;
        b.set_lift_addr(Some(ADD));
        b.build_int_binary_operation(c3, c4, IntBinaryOp::Add, ValueType::int(64))
    })?;

    run_pass(&ConstantFold, &mut fg)?;

    let folded = fg.returned();
    let fp = fg.side_tables().asm_fingerprint(fg.producer(folded));
    assert!(
        fp.contains(&A) && fp.contains(&B),
        "const-eval result must absorb both dying operand fingerprints \
         ({A:#x}, {B:#x}); got {fp:?}"
    );
    assert!(fp.contains(&ADD), "and the root's own {ADD:#x}; got {fp:?}");
    Ok(())
}

/// A root left with no uses folds nothing, so the live constant equal to its
/// value absorbs none of its operands' fingerprints.
#[test]
fn a_use_less_root_leaves_a_shared_constant_alone() -> Result<()> {
    const A: u64 = 0xC0FF_EE01;
    const B: u64 = 0xC0FF_EE02;
    const SEVEN: u64 = 0xC0FF_EE04;
    let mut dead_add = None;
    let mut fg = make_fn(|b| {
        b.set_lift_addr(Some(A));
        let c3 = b.build_int_const(3u64, ValueType::int(64))?;
        b.set_lift_addr(Some(B));
        let c4 = b.build_int_const(4u64, ValueType::int(64))?;
        let add = b.build_int_binary_operation(c3, c4, IntBinaryOp::Add, ValueType::int(64))?;
        dead_add = Some(add);
        b.set_lift_addr(Some(SEVEN));
        b.build_int_const(7u64, ValueType::int(64))
    })?;
    let add = fg.producer(dead_add.expect("built"));
    let seven = fg.producer(fg.returned());
    let mut edit = crate::EditFunction::new(&mut fg);
    for rule in super::rules::build_rules() {
        assert!(rule.try_at(&mut edit, add)?.is_none());
    }
    let fp = fg.side_tables().asm_fingerprint(seven);
    assert!(!fp.contains(&A) && !fp.contains(&B), "got {fp:?}");
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
        run_pass(&ConstantFold, &mut fg)?;
        let ret = fg.returned();
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
            rule.try_at(&mut edit, add)?.is_none(),
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
            rule.try_at(&mut edit, neg)?.is_none(),
            "rule {i} folded a `Neg` over a wider constant"
        );
    }
    Ok(())
}

/// Reassociating constants across a mixed-width nest drops the inner op's
/// wraparound: `Add(Add(x:I8, C1:I8):I8, C2:I16):I16` is not one `I16` add.
/// `validate` rejects the outer node, but a rewrite can mint one mid-pipeline.
#[test]
fn reassociation_refuses_a_mixed_width_nest() -> Result<()> {
    use strider_ir::node::ValueKind;
    for op in [
        IntBinaryOp::Add,
        IntBinaryOp::Mul,
        IntBinaryOp::And,
        IntBinaryOp::Xor,
    ] {
        let (mut fg, x) = make_fn_with_var(reg_vn(0x1000, 1), |b, _| {
            b.build_int_const(1u64, ValueType::int(16))
        })?;
        let mut edit = crate::EditFunction::new(&mut fg);
        let ret = return_value(edit.function().graph())?;
        let c1 = edit.build_int_const(0xF0u64, ValueType::int(8))?;
        let inner = edit.build_int_binary_operation(x, c1, op, ValueType::int(8))?;
        let c2 = edit.build_int_const(0x1F0u64, ValueType::int(16))?;
        let outer = edit.create_node(
            NodeKind::IntBinaryOp(op),
            [inner, c2],
            [ValueKind::Typed(ValueType::int(16))],
        );
        let [out] = edit.node_outputs_exact::<1>(outer)?;
        assert!(edit.replace_value(ret, out), "the return reads the nest");
        for (i, rule) in super::rules::build_rules().into_iter().enumerate() {
            assert!(
                rule.try_at(&mut edit, outer)?.is_none(),
                "rule {i} rewrote a mixed-width {op:?} nest"
            );
        }
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
            rule.try_at(&mut edit, add)?.is_none(),
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

/// An integer op over constants folds to its value at the declared width.
#[test]
fn int_ops_over_constants_fold() -> Result<()> {
    #[derive(Clone, Copy)]
    enum Op {
        Bin(IntBinaryOp),
        Cmp(IntCmpOp),
        Neg,
    }
    use IntBinaryOp::{Add, And, Mul, SShiftRight, ShiftLeft, ShiftRight, Xor};
    let (i1, i8, i32, i64) = (
        ValueType::I1,
        ValueType::int(8),
        ValueType::int(32),
        ValueType::int(64),
    );
    #[rustfmt::skip]
    let cases = [
        ("add", Op::Bin(Add), 3u128, 4u128, i64, 7u128),
        ("and_zero", Op::Bin(And), 0xFF, 0, i64, 0),
        ("mul_by_one", Op::Bin(Mul), 5, 1, i64, 5),
        ("shl_u32", Op::Bin(ShiftLeft), 1, 4, i32, 0x10),
        ("shr_u8", Op::Bin(ShiftRight), 0x80, 7, i8, 1),
        // Xor with all-ones is bitwise NOT: `~49 = 0xFFFF_FFCE`, not `-49`.
        ("xor_all_ones_u32", Op::Bin(Xor), 49, u128::MAX, i32, 0xFFFF_FFCE),
        ("xor_all_ones_u8", Op::Bin(Xor), 0xAA, u128::MAX, i8, 0x55),
        ("xor_all_ones_of_zero", Op::Bin(Xor), 0, u128::MAX, i32, 0xFFFF_FFFF),
        // `Neg` is two's complement: `-50 = 0xFFFF_FFCE`, not `~50`.
        ("neg_u32", Op::Neg, 50, 0, i32, 0xFFFF_FFCE),
        ("neg_zero", Op::Neg, 0, 0, i64, 0),
        // Shifts by `bit_width - 1`, the last in-range amount.
        ("shl_by_31", Op::Bin(ShiftLeft), 1, 31, i32, 0x8000_0000),
        ("lshr_by_31", Op::Bin(ShiftRight), 0xFFFF_FFFF, 31, i32, 1),
        // Sign fill, not zero fill.
        ("ashr_by_31_negative", Op::Bin(SShiftRight), 0x8000_0000, 31, i32, 0xFFFF_FFFF),
        ("ashr_by_31_positive", Op::Bin(SShiftRight), 0x7FFF_FFFF, 31, i32, 0),
        // At I1 the result is masked to the 1-bit width: `1 + 1` wraps to 0.
        ("i1_add", Op::Bin(Add), 1, 1, i1, 0),
        ("i1_and", Op::Bin(And), 1, 1, i1, 1),
        ("i1_and_false", Op::Bin(And), 1, 0, i1, 0),
        ("i1_xor", Op::Bin(Xor), 1, 1, i1, 0),
        ("equal", Op::Cmp(IntCmpOp::Equal), 5, 5, i64, 1),
        ("less", Op::Cmp(IntCmpOp::Less), 3, 5, i64, 1),
        // An I128 constant too wide for `u64`.
        ("i128_add", Op::Bin(Add), 1 << 100, 1 << 101, ValueType::int(128), 3 << 100),
    ];
    for (case, op, lhs, rhs, ty, want) in cases {
        let mut fg = make_fn(|b| {
            let l = b.build_int_const(lhs, ty)?;
            match op {
                Op::Bin(op) => {
                    let r = b.build_int_const(rhs, ty)?;
                    b.build_int_binary_operation(l, r, op, ty)
                }
                Op::Cmp(op) => {
                    let r = b.build_int_const(rhs, ty)?;
                    b.build_int_cmp_operation(l, r, op, ty)
                }
                Op::Neg => b.build_int_unary_operation(l, IntUnaryOp::Neg, ty),
            }
        })?;
        assert!(run_pass(&ConstantFold, &mut fg)?, "{case}: must fold");
        assert_eq!(fg.int_const_u128(fg.returned()), Some(want), "{case}");
    }
    Ok(())
}

/// `Xor(C, C)` and the lowered `Add(C, Neg(C))` with ONE interned constant on
/// both inputs fold to 0.
#[test]
fn fold_an_op_of_one_constant_twice() -> Result<()> {
    for sub in [false, true] {
        let mut fg = make_fn(|b| {
            let x = b.build_int_const(0xABu64, ValueType::int(64))?;
            if sub {
                b.build_sub_as_add_neg(x, x, ValueType::int(64))
            } else {
                b.build_int_binary_operation(x, x, IntBinaryOp::Xor, ValueType::int(64))
            }
        })?;
        assert!(run_pass(&ConstantFold, &mut fg)?, "sub={sub}");
        assert_returns_const(&fg, 0);
    }
    Ok(())
}

/// `x + 0` and `x - 0`, lowered to `Add(x, Neg(0))`, are `x`.
#[test]
fn fold_add_or_sub_zero_to_x() -> Result<()> {
    for sub in [false, true] {
        let (mut fg, x) = make_fn_with_var(reg_vn(0x1000, 8), |b, x| {
            let zero = b.build_int_const(0u64, ValueType::int(64))?;
            if sub {
                b.build_sub_as_add_neg(x, zero, ValueType::int(64))
            } else {
                b.build_int_binary_operation(x, zero, IntBinaryOp::Add, ValueType::int(64))
            }
        })?;
        run_to_fixed_point(&ConstantFold, &mut fg)?;
        assert_eq!(fg.returned(), x, "sub={sub}");
    }
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
    let outcome = run_pass(&ConstantFold, &mut fg)?;
    assert!(outcome, "wide-const unary fold must fold");
    let ret = fg.returned();
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
        let c = b.build_int_const(5u64, ValueType::int(64))?;
        // Const on the LEFT.
        b.build_int_binary_operation(c, x, IntBinaryOp::Add, ValueType::int(64))
    })?;
    run_to_fixed_point(&ConstantFold, &mut fg)?;
    let ret = fg.returned();
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

/// A chain of constant steps over `x` reassociates into one op whose
/// constant is the steps' combined value: `(x op C1) op C2 -> x op (C1 op
/// C2)`, a lowered subtraction adding the negated constant. A step's
/// constant sits on the right unless the row puts it on the left.
#[test]
fn constant_chains_reassociate() -> Result<()> {
    #[derive(Clone, Copy, Debug)]
    enum Step {
        R(IntBinaryOp, u64),
        L(IntBinaryOp, u64),
        Sub(u64),
    }
    use IntBinaryOp::{Add, And, Mul, Xor};
    use Step::{L, R, Sub};
    #[rustfmt::skip]
    let cases: [(&str, u16, &[Step], IntBinaryOp, i128); 13] = [
        ("add_add", 64, &[R(Add, 3), R(Add, 4)], Add, 7),
        ("mul_mul", 64, &[R(Mul, 3), R(Mul, 4)], Mul, 12),
        ("mul_mul_wraps", 64, &[R(Mul, u64::MAX), R(Mul, 3)], Mul, -3),
        ("xor_xor", 64, &[R(Xor, 0xF0), R(Xor, 0x0F)], Xor, 0xFF),
        // Bit 2 is the only bit both masks keep.
        ("and_and", 64, &[R(And, 4), R(And, 7)], And, 4),
        ("sub_add", 64, &[Sub(3), R(Add, 4)], Add, 1),
        ("add_sub_wraps", 64, &[R(Add, 3), Sub(4)], Add, -1),
        ("sub_sub", 64, &[Sub(3), Sub(4)], Add, -7),
        ("add_commuted_inner", 64, &[L(Add, 3), R(Add, 4)], Add, 7),
        ("add_commuted_outer", 64, &[R(Add, 3), L(Add, 4)], Add, 7),
        ("three_subs", 64, &[Sub(4), Sub(4), Sub(4)], Add, -12),
        ("three_subs_u32", 32, &[Sub(4), Sub(4), Sub(4)], Add, -12),
        // Collapsed by the worklist re-enqueueing the rules along the way.
        ("ten_subs", 64, &[Sub(1); 10], Add, -10),
    ];
    for (case, bits, steps, op, want) in cases {
        let ty = ValueType::int(bits);
        let (mut fg, x) = make_fn_with_var(reg_vn(0x1000, u32::from(bits / 8)), |b, x| {
            steps.iter().try_fold(x, |acc, &step| match step {
                R(op, k) => b.build_const_binop(k, acc, op, ty),
                L(op, k) => {
                    let k = b.build_int_const(k, ty)?;
                    b.build_int_binary_operation(k, acc, op, ty)
                }
                Sub(k) => {
                    let k = b.build_int_const(k, ty)?;
                    b.build_sub_as_add_neg(acc, k, ty)
                }
            })
        })?;
        run_to_fixed_point(&ConstantFold, &mut fg)?;
        let node = fg.producer(fg.returned());
        assert_eq!(fg.node_kind(node), &NodeKind::IntBinaryOp(op), "{case}");
        let [l, r] = fg.node_inputs_exact::<2>(node)?;
        let want = strider_ir::eval::unsigned(strider_ir::IBig::from(want), ty);
        assert_eq!((l, fg.int_const(r)), (x, Some(&want)), "{case}");
        assert_eq!(fg.value_type(r)?, ty, "{case}");
    }
    Ok(())
}

/// `op(op(x:I32, c1), c2)` after ConstantFold: the shift the Return reads, as
/// `(op, count)` when its left operand is `x` and its count a constant.
fn shift_chain(op: IntBinaryOp, c1: u64, c2: u64) -> Result<Option<(NodeKind, u128)>> {
    let ty = ValueType::int(32);
    let (fg, x) = make_fn_with_var(reg_vn(0x1000, 4), |b, x| {
        let inner = b.build_const_binop(c1, x, op, ty)?;
        b.build_const_binop(c2, inner, op, ty)
    })?;
    let mut fg = fg;
    run_to_fixed_point(&ConstantFold, &mut fg)?;
    let ret = fg.returned();
    let node = fg.producer(ret);
    let [lhs, count] = fg.node_inputs_exact::<2>(node)?;
    Ok((lhs == x)
        .then(|| fg.int_const_u128(count).map(|k| (*fg.node_kind(node), k)))
        .flatten())
}

/// Two constant shifts of one kind are one shift by the summed count while
/// the sum stays below the width; an arithmetic one saturates at `W - 1`.
#[test]
fn chained_constant_shifts_compose() -> Result<()> {
    use IntBinaryOp::{SShiftRight, ShiftLeft, ShiftRight};
    for op in [ShiftLeft, ShiftRight, SShiftRight] {
        assert_eq!(
            shift_chain(op, 3, 5)?,
            Some((NodeKind::IntBinaryOp(op), 8)),
            "{op:?}"
        );
    }
    // At the width a logical chain is 0, not a shift by 32.
    for op in [ShiftLeft, ShiftRight] {
        assert_eq!(shift_chain(op, 20, 12)?, None, "{op:?}");
    }
    assert_eq!(
        shift_chain(SShiftRight, 20, 20)?,
        Some((NodeKind::IntBinaryOp(SShiftRight), 31))
    );
    Ok(())
}

/// The identities the shift-chain rules rest on, over every 8-bit value and
/// every count pair up to past the width.
#[test]
fn shift_chain_identities_hold_at_8_bits() {
    use strider_ir::eval::int_binary;
    let ty = ValueType::int(8);
    let u = |v: u32| UBig::from(v);
    for op in [
        IntBinaryOp::ShiftLeft,
        IntBinaryOp::ShiftRight,
        IntBinaryOp::SShiftRight,
    ] {
        for v in 0..256 {
            for a in 0..12 {
                for b in 0..12 {
                    let chained =
                        int_binary(op, &int_binary(op, &u(v), &u(a), ty).unwrap(), &u(b), ty);
                    let count = if op == IntBinaryOp::SShiftRight {
                        (a + b).min(7)
                    } else if a + b < 8 {
                        a + b
                    } else {
                        continue;
                    };
                    assert_eq!(
                        chained,
                        int_binary(op, &u(v), &u(count), ty),
                        "{op:?} {v} {a} {b}"
                    );
                }
            }
        }
    }
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
    let mut fg = b.build()?;
    let before = fg.returned();
    let res = run_pass(&ConstantFold, &mut fg)?;
    assert!(!res, "no-const chain should not reassociate");
    assert_eq!(fg.returned(), before);
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
    let k1 = b.build_int_const(c1, ValueType::int(64))?;
    let k2 = b.build_int_const(c2, ValueType::int(64))?;
    let k3 = b.build_int_const(c3, ValueType::int(64))?;
    let a_and = b.build_int_binary_operation(a, k1, IntBinaryOp::And, ValueType::int(64))?;
    let b_and = b.build_int_binary_operation(bval, k2, IntBinaryOp::And, ValueType::int(64))?;
    let or_node =
        b.build_int_binary_operation(a_and, b_and, IntBinaryOp::Or, ValueType::int(64))?;
    let outer = b.build_int_binary_operation(or_node, k3, IntBinaryOp::And, ValueType::int(64))?;
    b.build_return(Some(outer), &[])?;
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
    let changed = run_pass(&ConstantFold, &mut fg)?;
    assert!(changed, "distribution rule should fire and simplify");
    run_to_fixed_point(&ConstantFold, &mut fg)?;
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
    let mut fg = b.build()?;
    assert_eq!(reachable_or_nodes(&fg), 1, "test setup expects one Or node");

    let changed = run_pass(&ConstantFold, &mut fg)?;
    assert!(changed, "alignment-OR-removal rule should fire");
    run_to_fixed_point(&ConstantFold, &mut fg)?;
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
            rule.try_at(&mut edit, and_node)?.is_none(),
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
    run_pass(&ConstantFold, &mut fg)?;

    assert_eq!(
        reachable_or_nodes(&fg),
        1,
        "the factored Or shape must be preserved (no churn)",
    );

    let second = run_pass(&ConstantFold, &mut fg)?;
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
        let wide = b.build_int_const(0xFF00u64, ValueType::int(16))?;
        b.truncate_if_needed(wide, ValueType::int(8))
    })?;
    let val = fg.returned();
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
        let a = b.build_int_const(0xFFFFu64, ValueType::int(16))?;
        let b_ = b.build_int_const(0xFFFFu64, ValueType::int(16))?;
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

    assert!(run_pass(&ConstantFold, &mut fg)?);

    let val = fg.returned();
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

/// `Truncate_32(op(x:I32) to I64)` is `x`, for either extend.
#[test]
fn fold_truncate_of_extend_round_trip() -> Result<()> {
    for op in [ExtendOp::ZeroExtend, ExtendOp::SignExtend] {
        let (mut fg, x) = make_fn_with_var(reg_vn(0x1000, 4), |b, x| {
            let widened = b.extend_if_needed(x, ValueType::int(64), op)?;
            b.truncate_if_needed(widened, ValueType::int(32))
        })?;
        run_to_fixed_point(&ConstantFold, &mut fg)?;
        assert_eq!(fg.returned(), x, "{op:?}");
    }
    Ok(())
}

/// Nested casts of one kind are one cast: `ZExt(ZExt(x))`, `SExt(SExt(x))`
/// and `Trunc(Trunc(x))`.
#[test]
fn fold_nested_casts_collapse() -> Result<()> {
    for (bytes, kind, widths) in [
        (1, NodeKind::Extend(ExtendOp::ZeroExtend), [16, 32]),
        (1, NodeKind::Extend(ExtendOp::SignExtend), [16, 32]),
        (4, NodeKind::Truncate, [16, 8]),
    ] {
        let (mut fg, _) = make_fn_with_var(reg_vn(0x1000, bytes), |b, x| {
            widths.iter().try_fold(x, |v, &w| match kind {
                NodeKind::Extend(op) => b.extend_if_needed(v, ValueType::int(w), op),
                _ => b.truncate_if_needed(v, ValueType::int(w)),
            })
        })?;
        run_to_fixed_point(&ConstantFold, &mut fg)?;
        assert_eq!(fg.count_kind(|k| *k == kind), 1, "{kind:?}");
    }
    Ok(())
}

/// `Truncate_32(op(ext_a(a), rhs))` at 64 bits, `rhs` either `ext_b(b)` or
/// the constant `k`, with `a` and `b` each `bytes` wide, after ConstantFold.
/// Returns the function and the two reads.
fn truncated_binop(
    op: IntBinaryOp,
    [ext_a, ext_b]: [ExtendOp; 2],
    k: Option<u64>,
    bytes: u32,
) -> Result<(strider_ir::Function, ValueId, ValueId)> {
    let (av, bv) = (reg_vn(0x1000, bytes), reg_vn(0x1010, bytes));
    let mut b = RegisterSet::new()
        .tracked(av)
        .tracked(bv)
        .arg(av)
        .arg(bv)
        .build_fn_single_region()?;
    let (a, bval) = (b.read_variable(&av)?, b.read_variable(&bv)?);
    let wide = ValueType::int(64);
    let lhs = b.extend_if_needed(a, wide, ext_a)?;
    let rhs = match k {
        Some(k) => b.build_int_const(k, wide)?,
        None => b.extend_if_needed(bval, wide, ext_b)?,
    };
    let op_out = b.build_int_binary_operation(lhs, rhs, op, wide)?;
    let ret = b.truncate_if_needed(op_out, ValueType::int(32))?;
    b.build_return(Some(ret), &[])?;
    let mut fg = b.build()?;
    run_to_fixed_point(&ConstantFold, &mut fg)?;
    Ok((fg, a, bval))
}

/// The low 32 bits of these ops read only their operands' low 32 bits, so a
/// truncate of the op over extended 32-bit operands is the 32-bit op, with a
/// constant operand reduced mod 2^32. The sign-extended `Mul` is the MIPS32
/// `mul a, b` shape, whose truncate otherwise hides the `Mul` from a pattern.
#[test]
fn a_truncated_binop_over_extends_narrows() -> Result<()> {
    use ExtendOp::{SignExtend, ZeroExtend};
    const K: u64 = 0x1_2345_6789;
    let ops = [
        IntBinaryOp::Add,
        IntBinaryOp::Mul,
        IntBinaryOp::And,
        IntBinaryOp::Or,
        IntBinaryOp::Xor,
    ];
    let exts = [
        [ZeroExtend, ZeroExtend],
        [ZeroExtend, SignExtend],
        [SignExtend, ZeroExtend],
        [SignExtend, SignExtend],
    ];
    for op in ops {
        for ext in exts {
            for k in [None, Some(K)] {
                let (fg, a, bval) = truncated_binop(op, ext, k, 4)?;
                let ret = fg.returned();
                let node = fg.producer(ret);
                assert_eq!(
                    fg.node_kind(node),
                    &NodeKind::IntBinaryOp(op),
                    "{op:?} {ext:?} {k:?}"
                );
                assert_eq!(fg.value_type_opt(ret), Some(ValueType::int(32)));
                let ins: Vec<ValueId> = fg.node_inputs(node).into_iter().collect();
                let other = match k {
                    None => bval,
                    Some(_) => {
                        let c = ins.iter().copied().find(|&v| v != a).expect("a constant");
                        assert_eq!(fg.int_const_u128(c), Some(u128::from(K as u32)));
                        c
                    }
                };
                assert!(
                    ins.contains(&a) && ins.contains(&other),
                    "{op:?} {ext:?} {k:?}"
                );
            }
        }
    }
    Ok(())
}

/// A 16-bit operand under a 32-bit truncate keeps the truncate: its extend
/// is a real widening the 32-bit op would drop.
#[test]
fn a_truncated_binop_over_a_narrower_operand_stays() -> Result<()> {
    let (fg, _, _) = truncated_binop(
        IntBinaryOp::Add,
        [ExtendOp::ZeroExtend, ExtendOp::ZeroExtend],
        None,
        2,
    )?;
    assert_return_kind(fg.graph(), NodeKind::Truncate);
    Ok(())
}

/// `Truncate_32(merge(a, y))` over two 64-bit registers after ConstantFold,
/// and `a`.
fn truncated_merge(
    merge: impl FnOnce(&mut strider_ir::FunctionBuilder, ValueId, ValueId) -> Result<ValueId>,
) -> Result<(strider_ir::Function, ValueId)> {
    let (av, yv) = (reg_vn(0x1000, 8), reg_vn(0x1010, 8));
    let mut b = RegisterSet::new()
        .tracked(av)
        .tracked(yv)
        .arg(av)
        .arg(yv)
        .build_fn_single_region()?;
    let (a, y) = (b.read_variable(&av)?, b.read_variable(&yv)?);
    let merged = merge(&mut b, a, y)?;
    let ret = b.truncate_if_needed(merged, ValueType::int(32))?;
    b.build_return(Some(ret), &[])?;
    let mut fg = b.build()?;
    run_to_fixed_point(&ConstantFold, &mut fg)?;
    Ok((fg, a))
}

/// The return is `Truncate_32(a)`.
#[track_caller]
fn assert_returns_truncate_of(fg: &strider_ir::Function, a: ValueId) -> Result<()> {
    let node = fg.producer(fg.returned());
    assert_eq!(fg.node_kind(node), &NodeKind::Truncate);
    assert_eq!(fg.node_inputs_exact::<1>(node)?, [a]);
    Ok(())
}

/// The x86 `mov $eax, ...` register-merge cleanup: the high-mask half
/// contributes nothing below bit 32, so the truncate drops it. Either `Or`
/// orientation: the term that fails `and(...)` structurally sends the
/// matcher to the other order.
#[test]
fn fold_drop_high_half_in_or_truncate() -> Result<()> {
    let ty = ValueType::int(64);
    for and_on_left in [false, true] {
        let (fg, a) = truncated_merge(|b, a, y| {
            // Low 32 bits of the mask are zero.
            let mask = b.build_int_const(0xFFFF_FFFF_0000_0000u64, ty)?;
            let high = b.build_int_binary_operation(mask, y, IntBinaryOp::And, ty)?;
            let (l, r) = if and_on_left { (high, a) } else { (a, high) };
            b.build_int_binary_operation(l, r, IntBinaryOp::Or, ty)
        })?;
        assert_returns_truncate_of(&fg, a)?;
    }
    Ok(())
}

/// `Truncate_32(And(0xFFFF_FFFF, a)) -> Truncate_32(a)`, the constant on
/// either side: the `And`'s commutative retry binds it.
#[test]
fn fold_drop_low_mask_under_truncate() -> Result<()> {
    let ty = ValueType::int(64);
    for const_on_right in [false, true] {
        let (fg, a) = truncated_merge(|b, a, _| {
            let mask = b.build_int_const(0xFFFF_FFFFu64, ty)?;
            let (l, r) = if const_on_right { (a, mask) } else { (mask, a) };
            b.build_int_binary_operation(l, r, IntBinaryOp::And, ty)
        })?;
        assert_returns_truncate_of(&fg, a)?;
    }
    Ok(())
}

/// The round-trip rule must not fire when `x` is wider than the truncate's
/// output: `Truncate_16(ZExt_64(x:I32))` is a real I32->I16 narrowing, so it
/// becomes `Truncate_16(x)`, not `x`.
#[test]
fn fold_truncate_of_extend_skips_when_widths_differ() -> Result<()> {
    let (mut fg, x) = make_fn_with_var(reg_vn(0x1000, 4), |b, x| {
        let widened = b.extend_if_needed(x, ValueType::int(64), ExtendOp::ZeroExtend)?;
        b.truncate_if_needed(widened, ValueType::int(16))
    })?;
    run_to_fixed_point(&ConstantFold, &mut fg)?;
    let val = fg.returned();
    assert_eq!(fg.value_type_opt(val), Some(ValueType::int(16)));
    assert_returns_truncate_of(&fg, x)
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
            run_to_fixed_point(&ConstantFold, &mut opt)?;
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
                    entry: &|_| UBig::from(x),
                    rom: None,
                    seed: 0,
                    step_budget: 100,
                    private: None,
                    keeps: None,
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

/// `SExt(ZExt(x))` is `ZExt(x)` when the zext widens: its top bit is 0. The
/// other mixed nest, `ZExt(SExt(x))`, is two casts and stays.
#[test]
fn a_sign_extend_of_a_widening_zero_extend_is_one_zero_extend() -> Result<()> {
    use ExtendOp::{SignExtend, ZeroExtend};
    use strider_ir_test_utils::interp;
    for (inner, outer, kinds) in [
        (ZeroExtend, SignExtend, vec![NodeKind::Extend(ZeroExtend)]),
        (
            SignExtend,
            ZeroExtend,
            vec![NodeKind::Extend(SignExtend), NodeKind::Extend(ZeroExtend)],
        ),
    ] {
        let (raw, _) = make_fn_with_var(reg_vn(0x1000, 1), |b, x| {
            let mid = b.extend_if_needed(x, ValueType::int(32), inner)?;
            b.extend_if_needed(mid, ValueType::int(64), outer)
        })?;
        let mut opt = raw.clone();
        run_to_fixed_point(&ConstantFold, &mut opt)?;
        let mut left: Vec<NodeKind> = opt
            .walk()
            .map(|n| *opt.node_kind(n))
            .filter(|k| matches!(k, NodeKind::Extend(_)))
            .collect();
        left.sort_by_key(|k| format!("{k:?}"));
        assert_eq!(left, kinds, "{inner:?} then {outer:?}");
        for x in 0..=0xffu128 {
            let env = interp::Env {
                entry: &|_| UBig::from(x),
                rom: None,
                seed: 0,
                step_budget: 100,
                private: None,
                keeps: None,
            };
            assert_eq!(interp::run(&opt, &env), interp::run(&raw, &env), "x={x:#x}");
        }
    }
    Ok(())
}

// Booleans are 1-bit integers: NOT is `Xor(x, IntConst(1)):I1`, AND/OR/XOR are
// `IntBinaryOp` at I1, and a bool const is `IntConst(0|1)` typed I1. So these
// folds all flow through the generic integer rules.

/// `b ^ true` is already the canonical logical-NOT shape, so nothing rewrites
/// it, in either operand order.
#[test]
fn bool_xor_true_stays_a_not() -> Result<()> {
    for true_on_left in [false, true] {
        let (mut fg, _x) = make_fn_with_var(reg_vn(0x1000, 8), |b, x| {
            let c5 = b.build_int_const(5u64, ValueType::int(64))?;
            let cmp = b.build_int_cmp_operation(x, c5, IntCmpOp::Equal, ValueType::int(64))?;
            let t = b.build_boolean_const(true);
            let (l, r) = if true_on_left { (t, cmp) } else { (cmp, t) };
            b.build_int_binary_operation(l, r, IntBinaryOp::Xor, ValueType::I1)
        })?;
        run_pass(&ConstantFold, &mut fg)?;
        assert_return_kind(fg.graph(), NodeKind::IntBinaryOp(IntBinaryOp::Xor));
    }
    Ok(())
}

// The const is 0, not all-ones, so `x ^ 0 -> x` fires and collapses the Xor to
// the cmp.
#[test]
fn fold_bool_xor_false_to_x() -> Result<()> {
    let vn = reg_vn(0x1000, 8);
    let (mut fg, _x) = make_fn_with_var(vn, |b, x| {
        let c5 = b.build_int_const(5u64, ValueType::int(64))?;
        let cmp = b.build_int_cmp_operation(x, c5, IntCmpOp::Equal, ValueType::int(64))?;
        let f = b.build_boolean_const(false);
        b.build_int_binary_operation(cmp, f, IntBinaryOp::Xor, ValueType::I1)
    })?;
    assert!(run_pass(&ConstantFold, &mut fg)?);
    assert_return_kind(fg.graph(), NodeKind::IntCmpOp(IntCmpOp::Equal));
    Ok(())
}

// `x | true -> true` at I1, where `true` is the all-ones value.
#[test]
fn fold_bool_or_true_to_true() -> Result<()> {
    let vn = reg_vn(0x1000, 8);
    let (mut fg, _x) = make_fn_with_var(vn, |b, x| {
        let c5 = b.build_int_const(5u64, ValueType::int(64))?;
        // Non-const bool.
        let cmp = b.build_int_cmp_operation(x, c5, IntCmpOp::Equal, ValueType::int(64))?;
        let t = b.build_boolean_const(true);
        b.build_int_binary_operation(cmp, t, IntBinaryOp::Or, ValueType::I1)
    })?;
    assert!(run_pass(&ConstantFold, &mut fg)?);
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
        let all_ones = b.build_int_const(0xFFFF_FFFFu64, ValueType::int(32))?;
        b.build_int_binary_operation(x32, all_ones, IntBinaryOp::Or, ValueType::int(32))
    })?;
    assert!(run_pass(&ConstantFold, &mut fg)?);
    assert_returns_const(&fg, 0xFFFF_FFFF);
    Ok(())
}

// Each logical NOT at I1 is `Xor(_, IntConst(1))`, so this builds
// `Xor(Xor(cmp, 1), 1)`, which reassociation and `x ^ 0 -> x` collapse.
#[test]
fn fold_bool_double_not_to_x() -> Result<()> {
    let vn = reg_vn(0x1000, 8);
    let (mut fg, _x) = make_fn_with_var(vn, |b, x| {
        let c5 = b.build_int_const(5u64, ValueType::int(64))?;
        let cmp = b.build_int_cmp_operation(x, c5, IntCmpOp::Equal, ValueType::int(64))?;
        let one = b.build_int_const(u128::MAX, ValueType::I1)?;
        let n1 = b.build_int_binary_operation(cmp, one, IntBinaryOp::Xor, ValueType::I1)?;
        b.build_int_binary_operation(n1, one, IntBinaryOp::Xor, ValueType::I1)
    })?;
    assert!(run_pass(&ConstantFold, &mut fg)?);
    assert_return_kind(fg.graph(), NodeKind::IntCmpOp(IntCmpOp::Equal));
    Ok(())
}

/// Sleigh leaves division by zero undefined, so every divide-family op must
/// skip rather than fold.
#[test]
fn no_fold_divide_family_by_zero() -> Result<()> {
    for ty in [ValueType::int(32), ValueType::int(64)] {
        for op in [
            IntBinaryOp::Div,
            IntBinaryOp::Sdiv,
            IntBinaryOp::Rem,
            IntBinaryOp::Srem,
        ] {
            let mut fg = make_fn(|b| {
                let x = b.build_int_const(10u64, ty)?;
                b.build_const_binop(0u64, x, op, ty)
            })?;
            assert!(
                !run_pass(&ConstantFold, &mut fg)?,
                "{op:?} at {ty} by const 0 must not fold (undefined)",
            );
            assert_eq!(return_kind(fg.graph())?, NodeKind::IntBinaryOp(op));
        }
    }
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
            let v = b.build_int_const(c.input, c.ty)?;
            match c.op {
                CountOp::Popcount => b.build_popcount(v, c.ty),
                CountOp::Lzcount => b.build_lzcount(v, c.ty),
            }
        })?;
        assert!(
            run_pass(&ConstantFold, &mut fg)?,
            "{}: ConstantFold must fold",
            c.case,
        );
        {
            let ret_val = fg.returned();
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
    let mut fg = make_fn(|b| b.build_int_const(0u64, ValueType::int(64)))?;
    let placeholder = fg.returned();
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
        let outcome = run_pass(&ConstantFold, &mut fg)?;
        assert!(
            outcome,
            "{case}: ConstantFold must fold {kind:?}({wide_ty:?} const)"
        );
        assert_eq!(
            fg.int_const_u128(fg.returned()),
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
            run_pass(&ConstantFold, &mut fg)?,
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
            run_pass(&ConstantFold, &mut fg)?,
            "{}: ConstantFold must fold the two-const {:?}",
            c.case,
            c.op,
        );
        {
            let ret_val = fg.returned();
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
            run_pass(&ConstantFold, &mut fg)?,
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

/// `Round` rounds a tie away from zero, at both widths.
#[test]
fn fold_round_is_ties_away_from_zero() -> Result<()> {
    let cases = [
        (0.5, 1.0),
        (-0.5, -1.0),
        (1.5, 2.0),
        (-1.5, -2.0),
        (2.5, 3.0),
        (-2.5, -3.0),
        (3.5, 4.0),
        (-3.5, -4.0),
    ];
    for (input, expected) in cases {
        let as_f32 = |v: f64| u64::from((v as f32).to_bits());
        for (bits, enc) in [(64, f64::to_bits as fn(f64) -> u64), (32, as_f32)] {
            let ty = ValueType::float(bits);
            let mut fg = make_fn(|b| {
                let v = b.build_float_const(enc(input), ty);
                b.build_float_unary_op(v, FloatUnaryOp::Round, ty)
            })?;
            assert!(run_pass(&ConstantFold, &mut fg)?, "Round({input}) at {ty}");
            assert_eq!(
                return_kind(fg.graph())?,
                NodeKind::FloatConst(enc(expected)),
                "Round({input}) at {ty}",
            );
        }
    }
    Ok(())
}

/// `IntBitsToFloat(FloatBitsToInt(f)) -> f` over a converted register.
#[test]
fn fold_bitcast_identity_int_bits_to_float_of_float_bits_to_int() -> Result<()> {
    let ty = ValueType::float(64);
    let mut fg = make_fn_with_var(reg_vn(0x1000, 8), |b, x| {
        let f = b.build_int_to_float(x, ty)?;
        let as_int = b.build_float_bits_to_int(f, ValueType::int(64))?;
        b.build_int_bits_to_float(as_int, ty)
    })?
    .0;
    assert!(run_pass(&ConstantFold, &mut fg)?);
    assert_return_kind(fg.graph(), NodeKind::IntToFloat);
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
    assert!(!run_pass(&ConstantFold, &mut fg)?);
    let val = fg.returned();
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
    assert!(run_pass(&ConstantFold, &mut fg)?);
    let val = fg.returned();
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
    run_pass(&ConstantFold, &mut fg)?;
    assert_return_kind(fg.graph(), NodeKind::FloatConst(0x3f80_0000));

    let mut fg = make_fn(|b| {
        let one = b.build_float_const(0x3f80_0000, ValueType::float(32));
        let sum = b.build_float_binary_op(one, one, FloatBinaryOp::Add, ValueType::float(32))?;
        b.build_float_bits_to_int(sum, ValueType::int(32))
    })?;
    run_pass(&ConstantFold, &mut fg)?;
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
        run_pass(&ConstantFold, &mut fg)?;
        assert_return_kind(fg.graph(), NodeKind::FloatConst(k));

        let mut fg = make_fn(|b| {
            let c = b.build_float_const(k, float_ty);
            Ok(raw_bitcast(b, NodeKind::FloatBitsToInt, c, int_ty))
        })?;
        run_pass(&ConstantFold, &mut fg)?;
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
    run_pass(&ConstantFold, &mut fg)?;
    assert_return_kind(fg.graph(), NodeKind::IntBitsToFloat);
    Ok(())
}

/// `FloatBitsToInt(IntBitsToFloat(x)) -> x` at 32 bits.
#[test]
fn fold_bitcast_roundtrip_f32() -> Result<()> {
    let (mut fg, x) = make_fn_with_var(reg_vn(0x1000, 4), |b, x| {
        let as_float = b.build_int_bits_to_float(x, ValueType::float(32))?;
        b.build_float_bits_to_int(as_float, ValueType::int(32))
    })?;
    assert!(run_pass(&ConstantFold, &mut fg)?);
    assert_eq!(fg.returned(), x);
    Ok(())
}

/// Converging in ONE pass, without the outer pipeline loop, is what proves
/// consumers are re-enqueued onto the worklist after a rewrite.
#[test]
fn single_pass_propagates_through_chain() -> Result<()> {
    let mut fg = make_fn(|b| {
        let one = b.build_int_const(1u64, ValueType::int(32))?;
        let two = b.build_int_const(2u64, ValueType::int(32))?;
        let three = b.build_int_const(3u64, ValueType::int(32))?;
        let four = b.build_int_const(4u64, ValueType::int(32))?;
        let c1 = b.build_int_binary_operation(one, two, IntBinaryOp::Add, ValueType::int(32))?;
        let c2 = b.build_int_binary_operation(c1, three, IntBinaryOp::Add, ValueType::int(32))?;
        b.build_int_binary_operation(c2, four, IntBinaryOp::Add, ValueType::int(32))
    })?;

    run_pass(&ConstantFold, &mut fg)?;

    {
        let ret_val = fg.returned();
        assert!(
            fg.int_const_u128(ret_val) == Some(10),
            "expected single-pass convergence to IntConst(10), got {:?}",
            fg.int_const_u128(ret_val)
        );
    }
    Ok(())
}

/// The all-ones arm compares `c` against its own declared width's all-ones,
/// so an `I256` `2^128` is not it and an `I256` all-ones is.
#[test]
fn all_ones_rules_read_the_declared_width() -> Result<()> {
    let ty = ValueType::int(256);
    for op in [IntBinaryOp::And, IntBinaryOp::Or] {
        // `2^128`: all-ones only to a saturating 128-bit mask.
        let mut fg = make_fn(|b| {
            let addr = b.build_int_const(0xF00Du64, ValueType::int(64))?;
            let x = b.build_load(addr, rsleigh::VnSpace::RAM, ty)?;
            let c = b.build_int_const(UBig::from_words(&[0, 0, 1, 0]), ty)?;
            b.build_int_binary_operation(x, c, op, ty)
        })?;
        let outcome = run_pass(&ConstantFold, &mut fg)?;
        assert!(!outcome, "{op:?} with an I256 2^128 is no identity");

        // The real all-ones: `x & ~0 -> x`, `x | ~0 -> ~0`.
        let mut fg = make_fn(|b| {
            let addr = b.build_int_const(0xF00Du64, ValueType::int(64))?;
            let x = b.build_load(addr, rsleigh::VnSpace::RAM, ty)?;
            let c = b.build_int_const(UBig::from_words(&[u64::MAX; 4]), ty)?;
            b.build_int_binary_operation(x, c, op, ty)
        })?;
        let outcome = run_pass(&ConstantFold, &mut fg)?;
        assert!(outcome, "{op:?} with an I256 all-ones is an identity");
        let want = match op {
            IntBinaryOp::And => NodeKind::Load(rsleigh::VnSpace::RAM),
            _ => NodeKind::IntConst(strider_ir::node::IntConstId::from_u32(0)),
        };
        assert!(
            std::mem::discriminant(&return_kind(fg.graph())?) == std::mem::discriminant(&want),
            "{op:?} must collapse to {want:?}, got {:?}",
            return_kind(fg.graph())?
        );
    }
    Ok(())
}

/// The REASSOCIATION rules combine the two constants themselves rather than
/// through `eval::int_binary`. They do it in `UBig` and the intern masks to
/// the declared width, so `(x + 2^127) + 2^127` at `I256` folds to
/// `x + 2^128`, not to the `x + 0` a 128-bit carry-out would give.
#[test]
fn reassoc_add_folds_past_128_bits() -> Result<()> {
    let half: u128 = 1 << 127;
    let mut fg = make_fn(|b| {
        let addr = b.build_int_const(0xF00Du64, ValueType::int(64))?;
        let x = b.build_load(addr, rsleigh::VnSpace::RAM, ValueType::int(256))?;
        let c1 = b.build_int_const(half, ValueType::int(256))?;
        let inner = b.build_int_binary_operation(x, c1, IntBinaryOp::Add, ValueType::int(256))?;
        let c2 = b.build_int_const(half, ValueType::int(256))?;
        b.build_int_binary_operation(inner, c2, IntBinaryOp::Add, ValueType::int(256))
    })?;
    run_to_fixed_point(&ConstantFold, &mut fg)?;
    let root = fg.producer(fg.returned());
    assert!(
        matches!(fg.node_kind(root), NodeKind::IntBinaryOp(IntBinaryOp::Add)),
        "got {:?}",
        fg.node_kind(root)
    );
    assert!(
        fg.node_inputs(root)
            .into_iter()
            .any(|v| fg.int_const(v) == Some(&(UBig::ONE << 128usize))),
        "the two halves combine to 2^128 at I256"
    );
    Ok(())
}

/// The same on the `Mul` side: `(x * 2^64) * 2^64` at `I256` folds to
/// `x * 2^128`, where a 128-bit product would have given `x * 0`.
#[test]
fn reassoc_mul_folds_past_128_bits() -> Result<()> {
    let two_64: u128 = 1 << 64;
    let mut fg = make_fn(|b| {
        let addr = b.build_int_const(0xF00Du64, ValueType::int(64))?;
        let x = b.build_load(addr, rsleigh::VnSpace::RAM, ValueType::int(256))?;
        let c1 = b.build_int_const(two_64, ValueType::int(256))?;
        let inner = b.build_int_binary_operation(x, c1, IntBinaryOp::Mul, ValueType::int(256))?;
        let c2 = b.build_int_const(two_64, ValueType::int(256))?;
        b.build_int_binary_operation(inner, c2, IntBinaryOp::Mul, ValueType::int(256))
    })?;
    run_to_fixed_point(&ConstantFold, &mut fg)?;
    let root = fg.producer(fg.returned());
    assert!(
        matches!(fg.node_kind(root), NodeKind::IntBinaryOp(IntBinaryOp::Mul)),
        "got {:?}",
        fg.node_kind(root)
    );
    assert!(
        fg.node_inputs(root)
            .into_iter()
            .any(|v| fg.int_const(v) == Some(&(UBig::ONE << 128usize))),
        "the two factors combine to 2^128 at I256"
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
    assert!(run_pass(&ConstantFold, &mut fg)?, "I256 shift must fold");
    assert_eq!(
        fg.int_const(fg.returned()),
        Some(&(UBig::ONE << 130usize)),
        "1 << 130 at I256"
    );

    // `Carry(2^128 - 1, 1)` overflows 128 bits but not 256.
    let mut fg = make_fn(|b| {
        let l = b.build_int_const(u128::MAX, i256)?;
        let r = b.build_int_const(1u64, i256)?;
        b.build_int_cmp_operation(l, r, IntCmpOp::Carry, i256)
    })?;
    assert!(run_pass(&ConstantFold, &mut fg)?, "I256 Carry must fold");
    assert_eq!(
        fg.int_const_u128(fg.returned()),
        Some(0),
        "2^128 - 1 plus 1 does not carry out of 256 bits"
    );

    // A signed compare past the carrier reads the I256 sign bit.
    let mut fg = make_fn(|b| {
        let l = b.build_int_const(UBig::ONE << 255usize, i256)?;
        let r = b.build_int_const(2u64, i256)?;
        b.build_int_cmp_operation(l, r, IntCmpOp::Sless, i256)
    })?;
    assert!(run_pass(&ConstantFold, &mut fg)?, "I256 Sless must fold");
    assert_eq!(
        fg.int_const_u128(fg.returned()),
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
        b.build_const_binop(4u64, l, IntBinaryOp::Sdiv, i256)
    })?;
    assert!(run_pass(&ConstantFold, &mut fg)?, "I256 Sdiv must fold");
    assert_eq!(
        fg.int_const(fg.returned()),
        Some(&((UBig::ONE << 256usize) - (UBig::ONE << 198usize))),
        "-(2^200) / 4 at I256"
    );

    // INT_MIN / -1 has no I256 result.
    let mut fg = make_fn(|b| {
        let l = b.build_int_const(min, i256)?;
        b.build_const_binop(all_ones, l, IntBinaryOp::Sdiv, i256)
    })?;
    let outcome = run_pass(&ConstantFold, &mut fg)?;
    assert!(!outcome, "I256 INT_MIN / -1 has no value");
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
        // Built as a node rather than through `extend_if_needed`, which folds
        // a constant extend itself and would leave the pass nothing to do.
        Ok(b.build_single_output_pure(
            NodeKind::Extend(strider_ir::ExtendOp::SignExtend),
            [v],
            i512,
        ))
    })?;
    assert!(
        run_pass(&ConstantFold, &mut fg)?,
        "I256 -> I512 sign extend must fold"
    );
    assert_eq!(
        fg.int_const(fg.returned()),
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
    let val = fg.returned();
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

/// Collecting `x` against itself: each side of the sum or difference is
/// `x`, `x*C` or `x<<Z`, and the result is one `Mul` by the combined
/// coefficient. Two equal products are one node, so `x*3 + x*3` binds the
/// product arm on both sides rather than the bare `x` swallowing it.
#[test]
fn factor_forms_collect_the_coefficient() -> Result<()> {
    use FactorArm::{Bare, Mul, Shl};
    #[rustfmt::skip]
    let cases = [
        (Bare, Mul(2), false, 3),
        (Bare, Bare, false, 2),
        (Mul(3), Mul(3), false, 6),
        (Mul(3), Mul(5), false, 8),
        (Bare, Shl(3), false, 9),
        // `1 - 3`, wrapping at the width.
        (Bare, Mul(3), true, u128::from(u64::MAX) - 1),
        (Mul(9), Mul(4), true, 5),
    ];
    let ty = ValueType::int(64);
    for (left, right, subtract, want) in cases {
        let (mut fg, x) = factor_arms_shape(ty, left, right, subtract)?;
        run_to_fixed_point(&ConstantFold, &mut fg)?;
        assert_mul_with_const(&fg, x, want, ty)?;
    }
    Ok(())
}

/// x - x -> 0, which the identity rule reaches before the difference rule
/// would build an `x*0`.
#[test]
fn factor_sub_self_self_is_zero() -> Result<()> {
    let (mut fg, _x) = make_fn_with_var(reg_vn(0x1000, 8), |b, x| {
        b.build_sub_as_add_neg(x, x, ValueType::int(64))
    })?;
    run_to_fixed_point(&ConstantFold, &mut fg)?;
    let val = fg.returned();
    assert_eq!(fg.int_const_u128(val), Some(0));
    Ok(())
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
    run_to_fixed_point(&ConstantFold, &mut fg)?;
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
    run_to_fixed_point(&ConstantFold, &mut fg)?;
    assert_mul_with_const(&fg, x, 5, ValueType::int(64))
}

/// A shift count is an ordinary constant of the shifted type, so it can name a
/// value no `u128` can shift by. The rule must decline rather than evaluate
/// `1 << 200` and panic the whole lift.
#[test]
fn factor_declines_an_out_of_range_shift() -> Result<()> {
    let (mut fg, _x) = make_fn_with_var(reg_vn(0x1000, 8), |b, x| {
        let wide = b.build_int_const(200u64, ValueType::int(64))?;
        let s =
            b.build_int_binary_operation(x, wide, IntBinaryOp::ShiftLeft, ValueType::int(64))?;
        b.build_int_binary_operation(x, s, IntBinaryOp::Add, ValueType::int(64))
    })?;
    run_to_fixed_point(&ConstantFold, &mut fg)?;
    let add = fg.producer(fg.returned());
    assert_eq!(fg.node_kind(add), &NodeKind::IntBinaryOp(IntBinaryOp::Add));
    assert!(
        fg.node_inputs(add)
            .into_iter()
            .any(|v| *fg.kind_of_value(v) == NodeKind::IntBinaryOp(IntBinaryOp::ShiftLeft)),
        "a width-or-wider shift count must leave `x + (x << c)` standing"
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
        run_to_fixed_point(&ConstantFold, &mut fg)?;
        let val = fg.returned();
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
                run_to_fixed_point(&ConstantFold, &mut fg)?;
                // `linear_coeff` reads the unfolded nest as the same multiple.
                let root = fg.producer(fg.returned());
                assert!(
                    !(*fg.node_kind(root) == NodeKind::IntBinaryOp(IntBinaryOp::Mul)
                        && fg.node_inputs(root).into_iter().any(
                            |v| *fg.kind_of_value(v) == NodeKind::IntBinaryOp(IntBinaryOp::Mul)
                        )),
                    "{ty:?} ({c1:#x}, {c2:#x}) left the nest standing"
                );
                let got = linear_coeff(&fg, fg.returned(), base)
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
        let inner = b.build_const_binop(c1, base, IntBinaryOp::Mul, ty)?;
        b.build_const_binop(c2, inner, IntBinaryOp::Mul, ty)
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
            b.build_const_binop(k, base, op, ty)
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
                    run_to_fixed_point(&ConstantFold, &mut fg)?;
                    let val = fg.returned();
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
                    run_to_fixed_point(&ConstantFold, &mut fg)?;
                    // `linear_coeff` reads the unfolded `Add` as the same
                    // multiple.
                    let root = fg.producer(fg.returned());
                    assert_ne!(
                        *fg.node_kind(root),
                        NodeKind::IntBinaryOp(IntBinaryOp::Add),
                        "{form:?} at {ty:?} with ({c1:#x}, {c2:#x}) did not collapse"
                    );
                    let got = linear_coeff(&fg, fg.returned(), base)
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

/// The four non-shift forms compute their coefficient in `UBig` at the
/// declared width. `C = 2^128 - 1` is the coefficient that would expose a
/// 128-bit modulus: `C + 1` is 2^128 there, not 0.
#[test]
fn factor_folds_past_the_u128_carrier() -> Result<()> {
    use FactorForm::{AddMul, AddSelf, SubMul, SubSelf};
    let c1 = (UBig::ONE << 128usize) - UBig::ONE;
    for ty in [ValueType::int(256), ValueType::int(512)] {
        let modulus = UBig::ONE << ty.bit_width();
        for form in [AddSelf, AddMul, SubSelf, SubMul] {
            let want = match form {
                AddSelf => &c1 + UBig::ONE,
                AddMul => &c1 + UBig::from(3u8),
                SubSelf => &modulus + UBig::ONE - &c1,
                SubMul => &c1 - UBig::from(3u8),
            } % &modulus;
            let (mut fg, _base) = factor_shape(form, ty, u128::MAX, 3)?;
            run_to_fixed_point(&ConstantFold, &mut fg)?;
            let root = fg.producer(fg.returned());
            assert!(
                matches!(fg.node_kind(root), NodeKind::IntBinaryOp(IntBinaryOp::Mul)),
                "{form:?} at {ty:?} left {:?}",
                fg.node_kind(root)
            );
            assert!(
                fg.node_inputs(root)
                    .into_iter()
                    .any(|v| fg.int_const(v) == Some(&want)),
                "{form:?} at {ty:?} wants a coefficient of {want}"
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
        run_to_fixed_point(&ConstantFold, &mut fg)?;
        let ret = fg.returned();
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
        let Some(first_rule) = body.find("Rule::rewrite(") else {
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

/// A coefficient rule computes in `UBig` at the root's width, so it folds
/// past the carrier too: `C + 1` for the multiply form and `1 << C` for the
/// shift form are both read at the declared width.
#[test]
fn a_coefficient_rule_folds_past_the_carrier() -> Result<()> {
    for width in [64u16, 256] {
        let ty = ValueType::int(width);
        let vn = reg_vn(0x2000, u32::from(width) / 8);
        for (op, want) in [(IntBinaryOp::Mul, 4u8), (IntBinaryOp::ShiftLeft, 9)] {
            let (mut fg, _x) = make_fn_with_var(vn, |b, x| {
                let scaled = b.build_const_binop(3u64, x, op, ty)?;
                b.build_int_binary_operation(x, scaled, IntBinaryOp::Add, ty)
            })?;
            run_to_fixed_point(&ConstantFold, &mut fg)?;
            assert_eq!(
                return_kind(fg.graph())?,
                NodeKind::IntBinaryOp(IntBinaryOp::Mul),
                "x + (x {op:?} 3) at i{width}"
            );
            let root = fg.producer(fg.returned());
            assert!(
                fg.node_inputs(root)
                    .into_iter()
                    .any(|v| fg.int_const(v) == Some(&UBig::from(want))),
                "x + (x {op:?} 3) at i{width} wants a coefficient of {want}"
            );
        }
    }
    Ok(())
}

/// The `const_on_right` swap is held off `(C1, C2)` by a guard asking
/// whether `x` binds a constant. A value reader answers that off a carrier
/// width, so a 4-limb `I256` constant reads as a variable and the swap fires
/// in BOTH orientations, forever. The guard is structural, so it does not.
#[test]
fn the_swap_guard_sees_a_constant_wider_than_the_carrier() -> Result<()> {
    use strider_pattern::{Capture, MatchPat, Matcher, int_add, var};
    let ty = ValueType::int(256);
    let wide = UBig::from_words(&[1, 2, 3, 4]);
    let fg = make_fn(|b| {
        let c1 = b.build_int_const(wide.clone(), ty)?;
        let c2 = b.build_int_const(UBig::from_words(&[5, 6, 7, 8]), ty)?;
        b.build_int_binary_operation(c1, c2, IntBinaryOp::Add, ty)
    })?;
    let x = Capture::new();
    let pat = int_add(var(Capture::new()), var(x)).into_pattern();
    let hits = Matcher::new(&fg).find_all(&pat)?;
    // Both operand orders, so both wide constants reach the guard.
    assert_eq!(hits.len(), 2, "the Add commutes over two wide constants");
    for hit in &hits {
        let binds = hit.bindings();
        assert!(
            binds.get_value(x).is_some(),
            "the probe must bind an operand"
        );
        assert!(
            !super::rules::binds_no_int_const(binds, &fg, x),
            "a 4-limb I256 constant is a constant, whatever carrier reads it"
        );
    }
    Ok(())
}

/// The pass seeds exactly the kinds some rule roots at.
#[test]
fn seeds_the_kinds_its_rules_root_at() {
    for kind in crate::test_support::sample_kinds() {
        let old = matches!(
            kind,
            NodeKind::IntUnaryOp(_)
                | NodeKind::IntBinaryOp(_)
                | NodeKind::IntCmpOp(_)
                | NodeKind::Truncate
                | NodeKind::Popcount
                | NodeKind::Lzcount
                | NodeKind::Extend(_)
                | NodeKind::FloatBinaryOp(_)
                | NodeKind::FloatUnaryOp(_)
                | NodeKind::FloatCmpOp(_)
                | NodeKind::IntBitsToFloat
                | NodeKind::FloatBitsToInt
        );
        assert_eq!(super::RULES.with(|r| r.admits(&kind)), old, "{kind:?}");
    }
}
