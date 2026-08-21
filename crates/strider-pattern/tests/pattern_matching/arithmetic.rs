use strider_ir::{IntBinaryOp, IntCmpOp, IntUnaryOp};
use strider_pattern::*;

use super::support::{Tb, assertions as a, shapes};

#[test]
fn add_matches() {
    let function = shapes::add_consts(5, 3);
    a::matches(
        &function,
        int_add(int_const(5u128), int_const(3u128)).into_pattern(),
        1,
    );
}

#[test]
fn add_wrong_operand_rejects() {
    let function = shapes::add_consts(5, 3);
    a::none(
        &function,
        int_add(int_const(5u128), int_const(99u128)).into_pattern(),
    );
}

#[test]
fn every_int_binary_op_has_a_working_ctor() {
    type Ctor = fn() -> strider_pattern::matcher::Pattern;
    let ctor_add: Ctor = || int_add(int_const(5u128), int_const(3u128)).into_pattern();
    let ctor_mul: Ctor = || int_mul(int_const(5u128), int_const(3u128)).into_pattern();
    let ctor_div: Ctor = || int_div(int_const(5u128), int_const(3u128)).into_pattern();
    let ctor_sdiv: Ctor = || int_sdiv(int_const(5u128), int_const(3u128)).into_pattern();
    let ctor_rem: Ctor = || int_rem(int_const(5u128), int_const(3u128)).into_pattern();
    let ctor_srem: Ctor = || int_srem(int_const(5u128), int_const(3u128)).into_pattern();
    let ctor_and: Ctor = || int_and(int_const(5u128), int_const(3u128)).into_pattern();
    let ctor_or: Ctor = || int_or(int_const(5u128), int_const(3u128)).into_pattern();
    let ctor_xor: Ctor = || int_xor(int_const(5u128), int_const(3u128)).into_pattern();
    let ctor_shl: Ctor = || int_shl(int_const(5u128), int_const(3u128)).into_pattern();
    let ctor_shr: Ctor = || int_shr(int_const(5u128), int_const(3u128)).into_pattern();
    let ctor_sshr: Ctor = || int_sshr(int_const(5u128), int_const(3u128)).into_pattern();

    let cases: &[(IntBinaryOp, Ctor)] = &[
        (IntBinaryOp::Add, ctor_add),
        (IntBinaryOp::Mul, ctor_mul),
        (IntBinaryOp::Div, ctor_div),
        (IntBinaryOp::Sdiv, ctor_sdiv),
        (IntBinaryOp::Rem, ctor_rem),
        (IntBinaryOp::Srem, ctor_srem),
        (IntBinaryOp::And, ctor_and),
        (IntBinaryOp::Or, ctor_or),
        (IntBinaryOp::Xor, ctor_xor),
        (IntBinaryOp::ShiftLeft, ctor_shl),
        (IntBinaryOp::ShiftRight, ctor_shr),
        (IntBinaryOp::SShiftRight, ctor_sshr),
    ];

    for &(op, ctor) in cases {
        let function = shapes::int_bin_5_3(op);
        a::matches(&function, ctor(), 1);
    }
}

#[test]
fn wrong_op_rejects() {
    // Mul is a stand-in wrong op; the rejection check is op-agnostic.
    let function = shapes::int_bin_5_3(IntBinaryOp::Mul);
    a::none(
        &function,
        int_add(int_const(5u128), int_const(3u128)).into_pattern(),
    );
}

/// `int_sub(a, b)` is an alias for the lowered `Add(a, Neg(b))` shape.
#[test]
fn sub_matches_lowered_shape() {
    let mut t = Tb::empty();
    let l = t.u64(5);
    let r = t.u64(3);
    let lowered = t.sub(l, r);
    let function = t.ret_val(lowered);
    a::matches(
        &function,
        int_sub(int_const(5u128), int_const(3u128)).into_pattern(),
        1,
    );
}

/// `return(~5):I64`, where complement is the canonical
/// `Xor(IntConst(5), IntConst(all_ones))`.
fn int_bit_not_5() -> strider_ir::Function {
    let mut t = Tb::empty();
    let v = t.u64(5);
    let nv = t.bit_not_at(v, strider_ir::node::ValueType::I64);
    t.ret_val(nv)
}

#[test]
fn bit_not_matches() {
    let function = int_bit_not_5();
    a::matches(&function, int_not(int_const(5u128)).into_pattern(), 1);
}

#[test]
fn neg_matches() {
    let function = shapes::int_un(5, IntUnaryOp::Neg);
    a::matches(&function, int_neg(int_const(5u128)).into_pattern(), 1);
}

#[test]
fn popcount_matches() {
    let mut t = Tb::empty();
    let c = t.u64(5);
    let p = t.popcount(c);
    let function = t.ret_val(p);
    a::matches(&function, int_popcount(int_const(5u128)).into_pattern(), 1);
}

#[test]
fn lzcount_matches() {
    let mut t = Tb::empty();
    let c = t.u64(5);
    let l = t.lzcount(c);
    let function = t.ret_val(l);
    a::matches(&function, int_lzcount(int_const(5u128)).into_pattern(), 1);
}

#[test]
fn bit_not_wrong_operand_rejects() {
    let function = int_bit_not_5();
    a::none(&function, int_not(int_const(99u128)).into_pattern());
}

#[test]
fn unary_wrong_op_rejects() {
    // The canonical bit-not shape is a binary Xor, so a unary neg pattern
    // must reject it.
    let function = int_bit_not_5();
    a::none(&function, int_neg(int_const(5u128)).into_pattern());
}

#[test]
fn every_int_cmp_op_has_a_working_ctor() {
    type Ctor = fn() -> strider_pattern::matcher::Pattern;
    let cases: &[(IntCmpOp, Ctor)] = &[
        (IntCmpOp::Equal, || {
            int_eq(int_const(5u128), int_const(3u128)).into_pattern()
        }),
        (IntCmpOp::Less, || {
            int_lt(int_const(5u128), int_const(3u128)).into_pattern()
        }),
        (IntCmpOp::Sless, || {
            int_slt(int_const(5u128), int_const(3u128)).into_pattern()
        }),
        (IntCmpOp::Carry, || {
            int_carry(int_const(5u128), int_const(3u128)).into_pattern()
        }),
        (IntCmpOp::Scarry, || {
            int_scarry(int_const(5u128), int_const(3u128)).into_pattern()
        }),
        (IntCmpOp::Sborrow, || {
            int_sborrow(int_const(5u128), int_const(3u128)).into_pattern()
        }),
    ];
    for &(op, ctor) in cases {
        let function = shapes::int_cmp_5_3(op);
        a::matches(&function, ctor(), 1);
    }
}

/// `IntCmpOp::LessEqual` is not an IR primitive, so `int_le(a, b)` is an alias
/// for the lowered `not(Less(b, a))` shape.
#[test]
fn int_le_matches_lowered_shape() {
    let function = shapes::int_le_lowered_5_3();
    a::matches(
        &function,
        int_le(int_const(5u128), int_const(3u128)).into_pattern(),
        1,
    );
}

/// Signed analogue of [`int_le_matches_lowered_shape`].
#[test]
fn int_sle_matches_lowered_shape() {
    let function = shapes::int_sle_lowered_5_3();
    a::matches(
        &function,
        int_sle(int_const(5u128), int_const(3u128)).into_pattern(),
        1,
    );
}

#[test]
fn cmp_wrong_op_rejects() {
    let function = shapes::int_cmp_5_3(IntCmpOp::Equal);
    a::none(
        &function,
        int_lt(int_const(5u128), int_const(3u128)).into_pattern(),
    );
}

#[test]
fn nested_add_three_levels_matches() {
    let function = shapes::add_nested_3(1, 2, 3);
    a::matches(
        &function,
        int_add(
            int_add(int_const(1u128), int_const(2u128)),
            int_const(3u128),
        )
        .into_pattern(),
        1,
    );
}

#[test]
fn nested_pattern_depth_five() {
    // (((((1+2)+3)+4)+5)+6)
    let mut t = Tb::empty();
    let a1 = t.u64(1);
    let a2 = t.u64(2);
    let s = t.add(a1, a2);
    let a3 = t.u64(3);
    let s = t.add(s, a3);
    let a4 = t.u64(4);
    let s = t.add(s, a4);
    let a5 = t.u64(5);
    let s = t.add(s, a5);
    let a6 = t.u64(6);
    let s = t.add(s, a6);
    let function = t.ret_val(s);

    a::matches(
        &function,
        int_add(
            int_add(
                int_add(
                    int_add(
                        int_add(int_const(1u128), int_const(2u128)),
                        int_const(3u128),
                    ),
                    int_const(4u128),
                ),
                int_const(5u128),
            ),
            int_const(6u128),
        )
        .into_pattern(),
        1,
    );

    a::none(
        &function,
        int_add(
            int_add(
                int_add(
                    int_add(
                        int_add(int_const(1u128), int_const(999u128)),
                        int_const(3u128),
                    ),
                    int_const(4u128),
                ),
                int_const(5u128),
            ),
            int_const(6u128),
        )
        .into_pattern(),
    );
}

#[test]
fn nested_any_partial_matches() {
    let function = shapes::add_nested_3(1, 2, 3);
    let inner = Capture::new();
    let m = a::unique(
        &function,
        int_add(anything().capture(inner), int_const(3u128)).into_pattern(),
    );
    assert!(m.value(inner).is_some());
}
