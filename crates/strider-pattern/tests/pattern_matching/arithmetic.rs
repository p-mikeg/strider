//! Integer binary / unary / comparison pattern matching.
//!
//! Covers every int op family constructor (`add`, `sub`, …, `neg`, `not`, …,
//! `int_eq`, `int_lt`, …) including deep nesting and wrong-op / wrong-operand
//! rejections.  Commutative-vs-ordered semantics live in `commutativity.rs`.

use strider_ir::{IntBinaryOp, IntCmpOp, IntUnaryOp};
use strider_pattern::*;

use super::support::{Tb, assertions as a, shapes};

// ── Integer binary ops ────────────────────────────────────────────────────────

#[test]
fn add_matches() {
    let function = shapes::add_consts(5, 3);
    a::matches(&function, add(int_const(5u128), int_const(3u128)).into_pattern(), 1);
}

#[test]
fn add_wrong_operand_rejects() {
    let function = shapes::add_consts(5, 3);
    a::none(&function, add(int_const(5u128), int_const(99u128)).into_pattern());
}

#[test]
fn every_int_binary_op_has_a_working_ctor() {
    // Each ctor builds `OP(IntConst(5), IntConst(3))` finalised to a `Pattern`.
    type Ctor = fn() -> strider_pattern::pattern::Pattern;
    let ctor_add: Ctor = || add(int_const(5u128), int_const(3u128)).into_pattern();
    let ctor_mul: Ctor = || mul(int_const(5u128), int_const(3u128)).into_pattern();
    let ctor_div: Ctor = || div(int_const(5u128), int_const(3u128)).into_pattern();
    let ctor_sdiv: Ctor = || sdiv(int_const(5u128), int_const(3u128)).into_pattern();
    let ctor_rem: Ctor = || rem(int_const(5u128), int_const(3u128)).into_pattern();
    let ctor_srem: Ctor = || srem(int_const(5u128), int_const(3u128)).into_pattern();
    let ctor_and: Ctor = || and(int_const(5u128), int_const(3u128)).into_pattern();
    let ctor_or: Ctor = || or(int_const(5u128), int_const(3u128)).into_pattern();
    let ctor_xor: Ctor = || xor(int_const(5u128), int_const(3u128)).into_pattern();
    let ctor_shl: Ctor = || shl(int_const(5u128), int_const(3u128)).into_pattern();
    let ctor_shr: Ctor = || shr(int_const(5u128), int_const(3u128)).into_pattern();
    let ctor_sshr: Ctor = || sshr(int_const(5u128), int_const(3u128)).into_pattern();

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
    // Use Mul as the "different op" graph (Sub no longer exists; the
    // pattern's wrong-op-rejection check is op-agnostic).
    let function = shapes::int_bin_5_3(IntBinaryOp::Mul);
    a::none(&function, add(int_const(5u128), int_const(3u128)).into_pattern());
}

/// `pattern::sub(a, b)` is an ergonomic alias that constructs the lowered
/// shape `Add(a, Neg(b))`.  Build the lowered shape directly and verify
/// the alias matches it.
#[test]
fn sub_matches_lowered_shape() {
    let mut t = Tb::empty();
    let l = t.u64(5);
    let r = t.u64(3);
    let lowered = t.sub(l, r); // Tb::sub builds Add(l, Neg(r)) directly.
    let function = t.ret_val(lowered);
    a::matches(&function, sub(int_const(5u128), int_const(3u128)).into_pattern(), 1);
}

// ── Integer unary ops ─────────────────────────────────────────────────────────

/// Helper: build `return(bit_not(IntConst(v))):I64` where bitwise complement
/// is the canonical `Xor(IntConst(v), IntConst(all_ones))` shape.
fn int_bit_not_5() -> strider_ir::Function {
    let mut t = Tb::empty();
    let v = t.u64(5);
    let nv = t.bit_not_at(v, strider_ir::node::ValueType::I64);
    t.ret_val(nv)
}

#[test]
fn bit_not_matches() {
    let function = int_bit_not_5();
    a::matches(&function, bit_not(int_const(5u128)).into_pattern(), 1);
}

#[test]
fn neg_matches() {
    let function = shapes::int_un(5, IntUnaryOp::Neg);
    a::matches(&function, neg(int_const(5u128)).into_pattern(), 1);
}

#[test]
fn popcount_matches() {
    let mut t = Tb::empty();
    let c = t.u64(5);
    let p = t.popcount(c);
    let function = t.ret_val(p);
    a::matches(&function, popcount(int_const(5u128)).into_pattern(), 1);
}

#[test]
fn lzcount_matches() {
    let mut t = Tb::empty();
    let c = t.u64(5);
    let l = t.lzcount(c);
    let function = t.ret_val(l);
    a::matches(&function, lzcount(int_const(5u128)).into_pattern(), 1);
}

#[test]
fn bit_not_wrong_operand_rejects() {
    let function = int_bit_not_5();
    a::none(&function, bit_not(int_const(99u128)).into_pattern());
}

#[test]
fn unary_wrong_op_rejects() {
    // `neg(int_const(5))` matches `IntUnaryOp::Neg(IntConst(5))`; the
    // canonical bit-not shape `Xor(5, all_ones)` is a binary op, so the
    // `neg` pattern must reject it.
    let function = int_bit_not_5();
    a::none(&function, neg(int_const(5u128)).into_pattern());
}

// ── Integer comparisons ───────────────────────────────────────────────────────

#[test]
fn every_int_cmp_op_has_a_working_ctor() {
    type Ctor = fn() -> strider_pattern::pattern::Pattern;
    let cases: &[(IntCmpOp, Ctor)] = &[
        (IntCmpOp::Equal, || int_eq(int_const(5u128), int_const(3u128)).into_pattern()),
        (IntCmpOp::Less, || int_lt(int_const(5u128), int_const(3u128)).into_pattern()),
        (IntCmpOp::Sless, || int_slt(int_const(5u128), int_const(3u128)).into_pattern()),
        (IntCmpOp::Carry, || int_carry(int_const(5u128), int_const(3u128)).into_pattern()),
        (IntCmpOp::Scarry, || int_scarry(int_const(5u128), int_const(3u128)).into_pattern()),
        (IntCmpOp::Sborrow, || int_sborrow(int_const(5u128), int_const(3u128)).into_pattern()),
    ];
    for &(op, ctor) in cases {
        let function = shapes::int_cmp_5_3(op);
        a::matches(&function, ctor(), 1);
    }
}

/// `int_le(a, b)` is an ergonomic alias for the lowered shape
/// `BoolNeg(IntLess(b, a))` — `IntCmpOp::LessEqual` is not a primitive
/// in the IR.  Build the lowered shape directly and verify the ctor
/// matches it.
#[test]
fn int_le_matches_lowered_shape() {
    let function = shapes::int_le_lowered_5_3();
    a::matches(&function, int_le(int_const(5u128), int_const(3u128)).into_pattern(), 1);
}

/// Signed analogue of [`int_le_matches_lowered_shape`].
#[test]
fn int_sle_matches_lowered_shape() {
    let function = shapes::int_sle_lowered_5_3();
    a::matches(&function, int_sle(int_const(5u128), int_const(3u128)).into_pattern(), 1);
}

#[test]
fn cmp_wrong_op_rejects() {
    let function = shapes::int_cmp_5_3(IntCmpOp::Equal);
    a::none(&function, int_lt(int_const(5u128), int_const(3u128)).into_pattern());
}

// ── Nested / deep patterns ────────────────────────────────────────────────────

#[test]
fn nested_add_three_levels_matches() {
    let function = shapes::add_nested_3(1, 2, 3);
    // Pattern: add(add(1, 2), 3) — matches the outer Add whose lhs is inner.
    a::matches(
        &function,
        add(add(int_const(1u128), int_const(2u128)), int_const(3u128)).into_pattern(),
        1,
    );
}

#[test]
fn nested_pattern_depth_five() {
    // Graph: (((((1+2)+3)+4)+5)+6)
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

    // Exact shape.
    a::matches(
        &function,
        add(
            add(
                add(add(add(int_const(1u128), int_const(2u128)), int_const(3u128)), int_const(4u128)),
                int_const(5u128),
            ),
            int_const(6u128),
        )
        .into_pattern(),
        1,
    );

    // Wrong const buried deep → reject.
    a::none(
        &function,
        add(
            add(
                add(add(add(int_const(1u128), int_const(999u128)), int_const(3u128)), int_const(4u128)),
                int_const(5u128),
            ),
            int_const(6u128),
        )
        .into_pattern(),
    );
}

#[test]
fn nested_any_partial_matches() {
    // (inner + 3) with any() captures — inner add can be any shape.
    let function = shapes::add_nested_3(1, 2, 3);
    let inner = Capture::new();
    let m = a::unique(&function, add(any().capture(inner), int_const(3u128)).into_pattern());
    // `inner` should point to the inner Add's value output.
    assert!(m.value(inner).is_some());
}
