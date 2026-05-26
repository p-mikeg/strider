//! Commutative matching: `add`, `mul`, `and`, `or`, `xor`,
//! `bool_and/or/xor`, `float_add`, `float_mul`.
//!
//! For each commutative ctor:
//!   * match succeeds on both canonical and swapped operand orders;
//!   * `.ordered()` forces the match to fail on swapped order;
//!   * duplicate matches for the same root are not emitted.
//!
//! Non-commutative ctors (`sub`, `div`, `shl`, …) — already tested positive
//! cases in `arithmetic.rs` — are rechecked here with swapped operands to
//! confirm they do NOT match.

use strider_analyze::pattern::*;
use strider_ir::{BoolBinaryOp, FloatBinaryOp, FloatCmpOp, IntBinaryOp};

use super::support::{Tb, assertions as a, shapes};

// ── Commutative int ops match swapped operands ───────────────────────────────

#[test]
fn add_commutes() {
    let function = shapes::int_bin(5, 3, IntBinaryOp::Add);
    a::matches(&function, add(int_const(3), int_const(5)), 1); // swapped
    a::matches(&function, add(int_const(5), int_const(3)), 1); // canonical
}

#[test]
fn mul_commutes() {
    let function = shapes::int_bin(7, 9, IntBinaryOp::Mul);
    a::matches(&function, mul(int_const(9), int_const(7)), 1);
}

#[test]
fn and_or_xor_commute() {
    let g_and = shapes::int_bin(0xF0, 0x0F, IntBinaryOp::And);
    let g_or = shapes::int_bin(0xF0, 0x0F, IntBinaryOp::Or);
    let g_xor = shapes::int_bin(0xF0, 0x0F, IntBinaryOp::Xor);
    a::matches(&g_and, and(int_const(0x0F), int_const(0xF0)), 1);
    a::matches(&g_or, or(int_const(0x0F), int_const(0xF0)), 1);
    a::matches(&g_xor, xor(int_const(0x0F), int_const(0xF0)), 1);
}

// ── `.ordered()` disables commutative retry ──────────────────────────────────

#[test]
fn ordered_rejects_swap() {
    let function = shapes::int_bin(5, 3, IntBinaryOp::Add);
    // Swapped order with `.ordered()` must fail.
    a::none(&function, add(int_const(3), int_const(5)).ordered());
    // But canonical order still matches.
    a::matches(&function, add(int_const(5), int_const(3)).ordered(), 1);
}

#[test]
fn ordered_mul_rejects_swap() {
    let function = shapes::int_bin(7, 9, IntBinaryOp::Mul);
    a::none(&function, mul(int_const(9), int_const(7)).ordered());
}

// ── No duplicate match from the swap retry ───────────────────────────────────

#[test]
fn commutative_match_emits_single_match_per_root() {
    // add(5, 3): pattern add(any(), any()) would in principle match twice if
    // the swap retry over-counted.  Exactly one match per root.
    let function = shapes::int_bin(5, 3, IntBinaryOp::Add);
    a::matches(&function, add(any(), any()), 1);
}

#[test]
fn commutative_match_with_identical_operands_emits_one() {
    // add(5, 5) — constant dedup means both operands share a NodeOutputId.
    // add(int_const(5), int_const(5)) must match exactly once.
    let function = shapes::int_bin(5, 5, IntBinaryOp::Add);
    a::matches(&function, add(int_const(5), int_const(5)), 1);
}

// ── Non-commutative ops REJECT swap ──────────────────────────────────────────

#[test]
fn sub_does_not_commute() {
    // `pattern::sub(a, b)` is an ergonomic alias for `Add(a, Neg(b))`.
    // Build the lowered `5 - 3` shape directly via the test helper and
    // verify operand order is preserved (i.e. swapping captures fails).
    let mut t = Tb::empty();
    let l = t.u64(5);
    let r = t.u64(3);
    let lowered = t.sub(l, r);
    let function = t.ret_val(lowered);
    a::none(&function, sub(int_const(3), int_const(5)));
    a::matches(&function, sub(int_const(5), int_const(3)), 1);
}

#[test]
fn div_shl_shr_do_not_commute() {
    let g_div = shapes::int_bin(20, 4, IntBinaryOp::Div);
    let g_shl = shapes::int_bin(1, 8, IntBinaryOp::ShiftLeft);
    let g_shr = shapes::int_bin(256, 2, IntBinaryOp::ShiftRight);

    a::none(&g_div, div(int_const(4), int_const(20)));
    a::none(&g_shl, shl(int_const(8), int_const(1)));
    a::none(&g_shr, shr(int_const(2), int_const(256)));
}

// ── Boolean commutativity ────────────────────────────────────────────────────

#[test]
fn bool_and_or_xor_commute() {
    let g_and = shapes::bool_bin(true, false, BoolBinaryOp::And);
    let g_or = shapes::bool_bin(true, false, BoolBinaryOp::Or);
    let g_xor = shapes::bool_bin(true, false, BoolBinaryOp::Xor);
    a::matches(&g_and, bool_and(bool_const(false), bool_const(true)), 1);
    a::matches(&g_or, bool_or(bool_const(false), bool_const(true)), 1);
    a::matches(&g_xor, bool_xor(bool_const(false), bool_const(true)), 1);
}

// ── Float commutativity ──────────────────────────────────────────────────────

#[test]
fn float_add_and_mul_commute() {
    let g_add = shapes::float_bin(2.0, 5.0, FloatBinaryOp::Add);
    let g_mul = shapes::float_bin(2.0, 5.0, FloatBinaryOp::Mul);
    a::matches(
        &g_add,
        float_add(float_const(5.0f64.to_bits()), float_const(2.0f64.to_bits())),
        1,
    );
    a::matches(
        &g_mul,
        float_mul(float_const(5.0f64.to_bits()), float_const(2.0f64.to_bits())),
        1,
    );
}

#[test]
fn float_sub_and_div_do_not_commute() {
    // `FloatBinaryOp::Sub` is no longer a primitive — `pattern::float_sub`
    // is an ergonomic alias that constructs the lowered shape
    // `FloatAdd(a, FloatUnaryOp::Neg(b))`.
    let g_sub = {
        let mut t = Tb::empty();
        let a = t.f64(5.0);
        let b = t.f64(2.0);
        let neg_b = t.fun(b, strider_ir::FloatUnaryOp::Neg, strider_ir::node::NodeOutputType::F64);
        let lowered = t.fbin(a, neg_b, FloatBinaryOp::Add, strider_ir::node::NodeOutputType::F64);
        let as_int = t.float_to_int(lowered, strider_ir::node::NodeOutputType::U64);
        t.ret_val(as_int)
    };
    a::none(
        &g_sub,
        float_sub(float_const(2.0f64.to_bits()), float_const(5.0f64.to_bits())),
    );

    let g_div = shapes::float_bin(10.0, 4.0, FloatBinaryOp::Div);
    a::none(
        &g_div,
        float_div(float_const(4.0f64.to_bits()), float_const(10.0f64.to_bits())),
    );
}

// ── Mixed commutative + non-commutative nesting ──────────────────────────────

/// Graph: `add(sub(a, b), c)`.  Commutative outer `add` can rearrange
/// `(sub-result, c)`, but non-commutative inner `sub` cannot.
#[test]
fn commutative_outer_non_commutative_inner() {
    let mut t = Tb::empty();
    let a = t.u64(10);
    let b = t.u64(3);
    let c = t.u64(5);
    let d = t.sub(a, b);
    let s = t.add(d, c);
    let function = t.ret_val(s);

    // Canonical shape.
    a::matches(
        &function,
        add(sub(int_const(10), int_const(3)), int_const(5)),
        1,
    );
    // Outer-add swapped: still matches (commutative).
    a::matches(
        &function,
        add(int_const(5), sub(int_const(10), int_const(3))),
        1,
    );
    // Inner-sub swapped: must NOT match.
    a::none(&function, add(sub(int_const(3), int_const(10)), int_const(5)));
    // Inner swap combined with outer swap: still must NOT match.
    a::none(&function, add(int_const(5), sub(int_const(3), int_const(10))));
}

// ── Capture consistency across commutative swap ──────────────────────────────

/// If a commutative pattern partially binds variables on its first operand
/// order and fails on the other, the swap retry must start with clean
/// bindings — otherwise `var(x)` used twice could spuriously match when the
/// operands are distinct.
#[test]
fn commutative_swap_does_not_leak_bindings() {
    // add(5, 3): operands are distinct.  Pattern `add(var(x), var(x))` should
    // NOT match (the two operands are different `NodeOutputId`s and `x`
    // enforces identity).
    let function = shapes::int_bin(5, 3, IntBinaryOp::Add);
    let x = Capture::new();
    a::none(&function, add(var(x), var(x)));
}

#[test]
fn commutative_swap_matches_identical_operand_with_identity_capture() {
    // add(5, 5): constant dedup makes both operands the same output.  Now
    // `add(var(x), var(x))` MUST match.
    let function = shapes::int_bin(5, 5, IntBinaryOp::Add);
    let x = Capture::new();
    let m = a::unique(&function, add(var(x), var(x)));
    assert_eq!(m.get_uint(x, &function), Some(5));
}

// ── float_cmp commutativity ──────────────────────────────────────────────────

/// Builds a graph that asserts a float comparison `a OP b` and returns
/// the boolean result (cast to u64 for typability).
fn graph_float_cmp(l: f64, r: f64, op: FloatCmpOp) -> strider_ir::Function {
    let mut t = Tb::empty();
    let a = t.f64(l);
    let b = t.f64(r);
    let v = t.fcmp(a, b, op);
    let as_int = t.as_int(v, strider_ir::node::NodeOutputType::U64);
    t.ret_val(as_int)
}

#[test]
fn float_eq_commutes() {
    let function = graph_float_cmp(1.0, 2.0, FloatCmpOp::Equal);
    a::matches(&function, float_cmp(FloatCmpOp::Equal, float_const(2.0_f64.to_bits()), float_const(1.0_f64.to_bits())), 1);
    a::matches(&function, float_cmp(FloatCmpOp::Equal, float_const(1.0_f64.to_bits()), float_const(2.0_f64.to_bits())), 1);
}

#[test]
fn float_ne_commutes() {
    // `FloatCmpOp::NotEqual` is no longer a primitive — `pattern::float_ne`
    // is an ergonomic alias that constructs `BoolNeg(FloatEqual(_, _))`.
    let function = {
        let mut t = Tb::empty();
        let a = t.f64(1.0);
        let b = t.f64(2.0);
        let eq = t.fcmp(a, b, FloatCmpOp::Equal);
        let ne = t.bool_un(eq, strider_ir::BoolUnaryOp::Neg);
        let as_int = t.as_int(ne, strider_ir::node::NodeOutputType::U64);
        t.ret_val(as_int)
    };
    a::matches(&function, float_ne(float_const(2.0_f64.to_bits()), float_const(1.0_f64.to_bits())), 1);
    a::matches(&function, float_ne(float_const(1.0_f64.to_bits()), float_const(2.0_f64.to_bits())), 1);
}

#[test]
fn float_lt_does_not_commute() {
    let function = graph_float_cmp(1.0, 2.0, FloatCmpOp::Less);
    // Canonical order matches.
    a::matches(&function, float_cmp(FloatCmpOp::Less, float_const(1.0_f64.to_bits()), float_const(2.0_f64.to_bits())), 1);
    // Swapped order must NOT match — Less is directional.
    a::none(&function, float_cmp(FloatCmpOp::Less, float_const(2.0_f64.to_bits()), float_const(1.0_f64.to_bits())));
}
