//! Integer binary / unary / comparison pattern matching.
//!
//! Covers every int op family constructor (`add`, `sub`, …, `neg`, `not`, …,
//! `int_eq`, `int_lt`, …) including deep nesting and wrong-op / wrong-operand
//! rejections.  Commutative-vs-ordered semantics live in `commutativity.rs`.

use ir::{IntBinaryOp, IntCmpOp, IntUnaryOp};
use pattern::*;

use super::support::{Tb, assertions as a, shapes};

// ── Integer binary ops ────────────────────────────────────────────────────────

#[test]
fn add_matches() {
    let g = shapes::add_consts(5, 3);
    a::matches(&g, add(int_const(5), int_const(3)), 1);
}

#[test]
fn add_wrong_operand_rejects() {
    let g = shapes::add_consts(5, 3);
    a::none(&g, add(int_const(5), int_const(99)));
}

#[test]
fn every_int_binary_op_has_a_working_ctor() {
    type Ctor = fn(Pat, Pat) -> Pat;
    let ctor_add: Ctor = |l, r| add(l, r).into();
    let ctor_mul: Ctor = |l, r| mul(l, r).into();
    let ctor_div: Ctor = |l, r| div(l, r).into();
    let ctor_sdiv: Ctor = |l, r| sdiv(l, r).into();
    let ctor_rem: Ctor = |l, r| rem(l, r).into();
    let ctor_srem: Ctor = |l, r| srem(l, r).into();
    let ctor_and: Ctor = |l, r| and(l, r).into();
    let ctor_or: Ctor = |l, r| or(l, r).into();
    let ctor_xor: Ctor = |l, r| xor(l, r).into();
    let ctor_shl: Ctor = |l, r| shl(l, r).into();
    let ctor_shr: Ctor = |l, r| shr(l, r).into();
    let ctor_sshr: Ctor = |l, r| sshr(l, r).into();

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
        let g = shapes::int_bin_5_3(op);
        a::matches(&g, ctor(int_const(5), int_const(3)), 1);
    }
}

#[test]
fn wrong_op_rejects() {
    // Use Mul as the "different op" graph (Sub no longer exists; the
    // pattern's wrong-op-rejection check is op-agnostic).
    let g = shapes::int_bin_5_3(IntBinaryOp::Mul);
    a::none(&g, add(int_const(5), int_const(3)));
}

/// `pattern::sub(a, b)` is an ergonomic alias that constructs the lowered
/// shape `Add(a, Neg(b))`.  Build the lowered shape directly and verify
/// the alias matches it.
#[test]
fn sub_matches_lowered_shape() {
    let mut t = Tb::empty();
    let l = t.u64(5);
    let r = t.u64(3);
    let lowered = t.sub(l, r);  // Tb::sub builds Add(l, Neg(r)) directly.
    let g = t.ret_val(lowered);
    a::matches(&g, sub(int_const(5), int_const(3)), 1);
}

// ── Integer unary ops ─────────────────────────────────────────────────────────

#[test]
fn bit_not_matches() {
    let g = shapes::int_un(5, IntUnaryOp::BitNot);
    a::matches(&g, bit_not(int_const(5)), 1);
}

#[test]
fn neg_matches() {
    let g = shapes::int_un(5, IntUnaryOp::Neg);
    a::matches(&g, neg(int_const(5)), 1);
}

#[test]
fn popcount_matches() {
    let mut t = Tb::empty();
    let c = t.u64(5);
    let p = t.popcount(c);
    let g = t.ret_val(p);
    a::matches(&g, popcount(int_const(5)), 1);
}

#[test]
fn lzcount_matches() {
    let mut t = Tb::empty();
    let c = t.u64(5);
    let l = t.lzcount(c);
    let g = t.ret_val(l);
    a::matches(&g, lzcount(int_const(5)), 1);
}

#[test]
fn bit_not_wrong_operand_rejects() {
    let g = shapes::int_un(5, IntUnaryOp::BitNot);
    a::none(&g, bit_not(int_const(99)));
}

#[test]
fn unary_wrong_op_rejects() {
    let g = shapes::int_un(5, IntUnaryOp::BitNot);
    a::none(&g, neg(int_const(5)));
}

// ── Integer comparisons ───────────────────────────────────────────────────────

#[test]
fn every_int_cmp_op_has_a_working_ctor() {
    type Ctor = fn(Pat, Pat) -> Pat;
    let cases: &[(IntCmpOp, Ctor)] = &[
        (IntCmpOp::Equal, |l, r| int_eq(l, r)),
        (IntCmpOp::Less, |l, r| int_lt(l, r)),
        (IntCmpOp::Sless, |l, r| int_slt(l, r)),
        (IntCmpOp::Carry, |l, r| int_carry(l, r)),
        (IntCmpOp::Scarry, |l, r| int_scarry(l, r)),
        (IntCmpOp::Sborrow, |l, r| int_sborrow(l, r)),
    ];
    for &(op, ctor) in cases {
        let g = shapes::int_cmp_5_3(op);
        a::matches(&g, ctor(int_const(5), int_const(3)), 1);
    }
}

/// `int_le(a, b)` is an ergonomic alias for the lowered shape
/// `BoolNeg(IntLess(b, a))` — `IntCmpOp::LessEqual` is not a primitive
/// in the IR.  Build the lowered shape directly and verify the ctor
/// matches it.
#[test]
fn int_le_matches_lowered_shape() {
    let g = shapes::int_le_lowered_5_3();
    a::matches(&g, int_le(int_const(5), int_const(3)), 1);
}

/// Signed analogue of [`int_le_matches_lowered_shape`].
#[test]
fn int_sle_matches_lowered_shape() {
    let g = shapes::int_sle_lowered_5_3();
    a::matches(&g, int_sle(int_const(5), int_const(3)), 1);
}

/// `pattern::float_le(a, b)` is an ergonomic alias for the lowered shape
/// `Or(FloatLess(a, b), FloatEqual(a, b))` (NaN-aware — the operand-swap-
/// and-negate trick used for `int_le` does not preserve `<=` on NaN).
/// Build the lowered shape directly and verify the alias matches it.
#[test]
fn float_le_matches_lowered_shape() {
    use ir::node::NodeOutputType;
    use ir::{BoolBinaryOp, FloatCmpOp};

    let mut t = Tb::empty();
    let l = t.f64(1.0);
    let r = t.f64(2.0);
    let lt = t.fcmp(l, r, FloatCmpOp::Less);
    let eq = t.fcmp(l, r, FloatCmpOp::Equal);
    let ored = t.bool_bin(lt, eq, BoolBinaryOp::Or);
    let cast = t.as_int(ored, NodeOutputType::U64);
    let g = t.ret_val(cast);
    a::matches(
        &g,
        float_le(float_const(1.0_f64.to_bits()), float_const(2.0_f64.to_bits())),
        1,
    );
}

#[test]
fn cmp_wrong_op_rejects() {
    let g = shapes::int_cmp_5_3(IntCmpOp::Equal);
    a::none(&g, int_lt(int_const(5), int_const(3)));
}

// ── Nested / deep patterns ────────────────────────────────────────────────────

#[test]
fn nested_add_three_levels_matches() {
    let g = shapes::add_nested_3(1, 2, 3);
    // Pattern: add(add(1, 2), 3) — matches the outer Add whose lhs is inner.
    a::matches(&g, add(add(int_const(1), int_const(2)), int_const(3)), 1);
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
    let g = t.ret_val(s);

    // Exact shape.
    a::matches(
        &g,
        add(
            add(
                add(add(add(int_const(1), int_const(2)), int_const(3)), int_const(4)),
                int_const(5),
            ),
            int_const(6),
        ),
        1,
    );

    // Wrong const buried deep → reject.
    a::none(
        &g,
        add(
            add(
                add(add(add(int_const(1), int_const(999)), int_const(3)), int_const(4)),
                int_const(5),
            ),
            int_const(6),
        ),
    );
}

#[test]
fn nested_any_partial_matches() {
    // (inner + 3) with any() captures — inner add can be any shape.
    let g = shapes::add_nested_3(1, 2, 3);
    let inner = Capture::new();
    let m = a::unique(&g, add(any().capture(inner), int_const(3)));
    // `inner` should point to the inner Add's value output.
    assert!(m.output(inner).is_some());
}
