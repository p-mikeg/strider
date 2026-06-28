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

use strider_ir::{FloatBinaryOp, FloatCmpOp, IRBuilderExt, IntBinaryOp, IntCmpOp};
use strider_pattern::*;

use super::support::{Tb, assertions as a, shapes};

// ── Commutative int ops match swapped operands ───────────────────────────────

#[test]
fn add_commutes() {
    let function = shapes::int_bin(5, 3, IntBinaryOp::Add);
    a::matches_both_orders(
        &function,
        add(int_const(5u128), int_const(3u128)).into_pattern(), // canonical
        add(int_const(3u128), int_const(5u128)).into_pattern(), // swapped
    );
}

#[test]
fn mul_commutes() {
    let function = shapes::int_bin(7, 9, IntBinaryOp::Mul);
    a::matches_both_orders(
        &function,
        mul(int_const(7u128), int_const(9u128)).into_pattern(),
        mul(int_const(9u128), int_const(7u128)).into_pattern(),
    );
}

#[test]
fn and_or_xor_commute() {
    let g_and = shapes::int_bin(0xF0, 0x0F, IntBinaryOp::And);
    let g_or = shapes::int_bin(0xF0, 0x0F, IntBinaryOp::Or);
    let g_xor = shapes::int_bin(0xF0, 0x0F, IntBinaryOp::Xor);
    a::matches_both_orders(
        &g_and,
        and(int_const(0xF0u128), int_const(0x0Fu128)).into_pattern(),
        and(int_const(0x0Fu128), int_const(0xF0u128)).into_pattern(),
    );
    a::matches_both_orders(
        &g_or,
        or(int_const(0xF0u128), int_const(0x0Fu128)).into_pattern(),
        or(int_const(0x0Fu128), int_const(0xF0u128)).into_pattern(),
    );
    a::matches_both_orders(
        &g_xor,
        xor(int_const(0xF0u128), int_const(0x0Fu128)).into_pattern(),
        xor(int_const(0x0Fu128), int_const(0xF0u128)).into_pattern(),
    );
}

// ── `.ordered()` disables commutative retry ──────────────────────────────────

#[test]
fn ordered_rejects_swap() {
    let function = shapes::int_bin(5, 3, IntBinaryOp::Add);
    // Swapped order with `.ordered()` must fail.
    a::none(
        &function,
        add(int_const(3u128), int_const(5u128))
            .ordered()
            .into_pattern(),
    );
    // But canonical order still matches.
    a::matches(
        &function,
        add(int_const(5u128), int_const(3u128))
            .ordered()
            .into_pattern(),
        1,
    );
}

#[test]
fn ordered_mul_rejects_swap() {
    let function = shapes::int_bin(7, 9, IntBinaryOp::Mul);
    a::none(
        &function,
        mul(int_const(9u128), int_const(7u128))
            .ordered()
            .into_pattern(),
    );
}

// ── No duplicate match from the swap retry ───────────────────────────────────

#[test]
fn commutative_match_emits_single_match_per_root() {
    // add(5, 3): pattern add(any(), any()) would in principle match twice if
    // the swap retry over-counted.  Exactly one match per root.
    let function = shapes::int_bin(5, 3, IntBinaryOp::Add);
    a::matches(&function, add(any(), any()).into_pattern(), 1);
}

#[test]
fn commutative_match_with_identical_operands_emits_one() {
    // add(5, 5) — constant dedup means both operands share a ValueId.
    // add(int_const(5), int_const(5)) must match exactly once.
    let function = shapes::int_bin(5, 5, IntBinaryOp::Add);
    a::matches(
        &function,
        add(int_const(5u128), int_const(5u128)).into_pattern(),
        1,
    );
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
    a::none(
        &function,
        sub(int_const(3u128), int_const(5u128)).into_pattern(),
    );
    a::matches(
        &function,
        sub(int_const(5u128), int_const(3u128)).into_pattern(),
        1,
    );
}

#[test]
fn div_shl_shr_do_not_commute() {
    let g_div = shapes::int_bin(20, 4, IntBinaryOp::Div);
    let g_shl = shapes::int_bin(1, 8, IntBinaryOp::ShiftLeft);
    let g_shr = shapes::int_bin(256, 2, IntBinaryOp::ShiftRight);

    a::none(
        &g_div,
        div(int_const(4u128), int_const(20u128)).into_pattern(),
    );
    a::none(
        &g_shl,
        shl(int_const(8u128), int_const(1u128)).into_pattern(),
    );
    a::none(
        &g_shr,
        shr(int_const(2u128), int_const(256u128)).into_pattern(),
    );
}

// ── Boolean commutativity ────────────────────────────────────────────────────

#[test]
fn bool_and_or_xor_commute() {
    let g_and = shapes::bool_bin(true, false, IntBinaryOp::And);
    let g_or = shapes::bool_bin(true, false, IntBinaryOp::Or);
    let g_xor = shapes::bool_bin(true, false, IntBinaryOp::Xor);
    a::matches_both_orders(
        &g_and,
        bool_and(bool_const(true), bool_const(false)).into_pattern(),
        bool_and(bool_const(false), bool_const(true)).into_pattern(),
    );
    a::matches_both_orders(
        &g_or,
        bool_or(bool_const(true), bool_const(false)).into_pattern(),
        bool_or(bool_const(false), bool_const(true)).into_pattern(),
    );
    a::matches_both_orders(
        &g_xor,
        bool_xor(bool_const(true), bool_const(false)).into_pattern(),
        bool_xor(bool_const(false), bool_const(true)).into_pattern(),
    );
}

// ── `bool_binary` / `bool_and` are chainable (`.ordered()` pins operand slots) ─

#[test]
fn bool_binary_ordered_rejects_swap() {
    // Build `and(true, false)` at I1.  Default `bool_binary` matches
    // commutatively; `.ordered()` pins the operand slots so the swapped
    // operand order must fail while the canonical order still matches.
    let function = shapes::bool_bin(true, false, IntBinaryOp::And);
    // Canonical (operand 0 = true, operand 1 = false) matches either way.
    a::matches(
        &function,
        bool_binary(IntBinaryOp::And, bool_const(true), bool_const(false)).into_pattern(),
        1,
    );
    // Without `.ordered()`, the swapped order still matches (commutative).
    a::matches(
        &function,
        bool_binary(IntBinaryOp::And, bool_const(false), bool_const(true)).into_pattern(),
        1,
    );
    // With `.ordered()`, the swapped order must NOT match…
    a::none(
        &function,
        bool_binary(IntBinaryOp::And, bool_const(false), bool_const(true))
            .ordered()
            .into_pattern(),
    );
    // …but the canonical order still does.
    a::matches(
        &function,
        bool_binary(IntBinaryOp::And, bool_const(true), bool_const(false))
            .ordered()
            .into_pattern(),
        1,
    );
}

#[test]
fn bool_and_ordered_rejects_swap() {
    let function = shapes::bool_bin(true, false, IntBinaryOp::And);
    a::none(
        &function,
        bool_and(bool_const(false), bool_const(true))
            .ordered()
            .into_pattern(),
    );
    a::matches(
        &function,
        bool_and(bool_const(true), bool_const(false))
            .ordered()
            .into_pattern(),
        1,
    );
}

/// The chainable `bool_binary` builder must keep the `I1` output guard:
/// `.ordered()` (or the bare builder) must NOT match a same-shaped wide
/// `And` even when the operand order lines up.
#[test]
fn bool_binary_ordered_requires_i1_output() {
    use strider_ir::node::ValueType;
    use strider_ir_test_utils::RegisterSet;

    let mut b = RegisterSet::new()
        .build_fn_single_region()
        .expect("build_fn_single_region");
    let x = b.build_int_const(0xFFu64, ValueType::I64).expect("x");
    let one = b.build_int_const(1u64, ValueType::I64).expect("one");
    let wide_and = b
        .build_int_binary_operation(x, one, IntBinaryOp::And, ValueType::I64)
        .expect("wide and");
    b.build_return(Some(wide_and), &[]).expect("ret");
    let function = b.build().expect("build");

    a::none(
        &function,
        bool_binary(IntBinaryOp::And, any(), any()).into_pattern(),
    );
    a::none(
        &function,
        bool_binary(IntBinaryOp::And, any(), any())
            .ordered()
            .into_pattern(),
    );
}

// ── int_cmp Carry / Scarry commutativity ─────────────────────────────────────
//
// `NodeKind::is_commutative` includes `IntCmpOp::Carry` and `IntCmpOp::Scarry`
// (an addition carry/overflow commutes because addition does), so `int_carry`
// / `int_scarry` must match swapped operands, and `.ordered()` must reject the
// swap.

#[test]
fn int_carry_commutes() {
    let function = shapes::int_cmp_5_3(IntCmpOp::Carry);
    a::matches_both_orders(
        &function,
        int_carry(int_const(5u128), int_const(3u128)).into_pattern(), // canonical
        int_carry(int_const(3u128), int_const(5u128)).into_pattern(), // swapped
    );
}

#[test]
fn ordered_int_carry_rejects_swap() {
    let function = shapes::int_cmp_5_3(IntCmpOp::Carry);
    // Swapped order with `.ordered()` must fail…
    a::none(
        &function,
        int_carry(int_const(3u128), int_const(5u128))
            .ordered()
            .into_pattern(),
    );
    // …but canonical order still matches.
    a::matches(
        &function,
        int_carry(int_const(5u128), int_const(3u128))
            .ordered()
            .into_pattern(),
        1,
    );
}

#[test]
fn int_scarry_commutes() {
    let function = shapes::int_cmp_5_3(IntCmpOp::Scarry);
    a::matches_both_orders(
        &function,
        int_scarry(int_const(5u128), int_const(3u128)).into_pattern(), // canonical
        int_scarry(int_const(3u128), int_const(5u128)).into_pattern(), // swapped
    );
}

#[test]
fn ordered_int_scarry_rejects_swap() {
    let function = shapes::int_cmp_5_3(IntCmpOp::Scarry);
    a::none(
        &function,
        int_scarry(int_const(3u128), int_const(5u128))
            .ordered()
            .into_pattern(),
    );
    a::matches(
        &function,
        int_scarry(int_const(5u128), int_const(3u128))
            .ordered()
            .into_pattern(),
        1,
    );
}

// ── Float commutativity ──────────────────────────────────────────────────────

#[test]
fn float_add_and_mul_commute() {
    let g_add = shapes::float_bin(2.0, 5.0, FloatBinaryOp::Add);
    let g_mul = shapes::float_bin(2.0, 5.0, FloatBinaryOp::Mul);
    a::matches_both_orders(
        &g_add,
        float_add(float_const(2.0f64.to_bits()), float_const(5.0f64.to_bits())).into_pattern(),
        float_add(float_const(5.0f64.to_bits()), float_const(2.0f64.to_bits())).into_pattern(),
    );
    a::matches_both_orders(
        &g_mul,
        float_mul(float_const(2.0f64.to_bits()), float_const(5.0f64.to_bits())).into_pattern(),
        float_mul(float_const(5.0f64.to_bits()), float_const(2.0f64.to_bits())).into_pattern(),
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
        let neg_b = t.fun(
            b,
            strider_ir::FloatUnaryOp::Neg,
            strider_ir::node::ValueType::F64,
        );
        let lowered = t.fbin(
            a,
            neg_b,
            FloatBinaryOp::Add,
            strider_ir::node::ValueType::F64,
        );
        let as_int = t.float_to_int(lowered, strider_ir::node::ValueType::I64);
        t.ret_val(as_int)
    };
    a::none(
        &g_sub,
        float_sub(float_const(2.0f64.to_bits()), float_const(5.0f64.to_bits())).into_pattern(),
    );

    let g_div = shapes::float_bin(10.0, 4.0, FloatBinaryOp::Div);
    a::none(
        &g_div,
        float_div(
            float_const(4.0f64.to_bits()),
            float_const(10.0f64.to_bits()),
        )
        .into_pattern(),
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
        add(sub(int_const(10u128), int_const(3u128)), int_const(5u128)).into_pattern(),
        1,
    );
    // Outer-add swapped: still matches (commutative).
    a::matches(
        &function,
        add(int_const(5u128), sub(int_const(10u128), int_const(3u128))).into_pattern(),
        1,
    );
    // Inner-sub swapped: must NOT match.
    a::none(
        &function,
        add(sub(int_const(3u128), int_const(10u128)), int_const(5u128)).into_pattern(),
    );
    // Inner swap combined with outer swap: still must NOT match.
    a::none(
        &function,
        add(int_const(5u128), sub(int_const(3u128), int_const(10u128))).into_pattern(),
    );
}

// ── Capture consistency across commutative swap ──────────────────────────────

/// If a commutative pattern partially binds variables on its first operand
/// order and fails on the other, the swap retry must start with clean
/// bindings — otherwise `var(x)` used twice could spuriously match when the
/// operands are distinct.
#[test]
fn commutative_swap_does_not_leak_bindings() {
    // add(5, 3): operands are distinct.  Pattern `add(var(x), var(x))` should
    // NOT match (the two operands are different `ValueId`s and `x`
    // enforces identity).
    let function = shapes::int_bin(5, 3, IntBinaryOp::Add);
    let x = Capture::new();
    a::none(&function, add(var(x), var(x)).into_pattern());
}

#[test]
fn commutative_swap_matches_identical_operand_with_identity_capture() {
    // add(5, 5): constant dedup makes both operands the same output.  Now
    // `add(var(x), var(x))` MUST match.
    let function = shapes::int_bin(5, 5, IntBinaryOp::Add);
    let x = Capture::new();
    assert_eq!(
        a::unique_uint(&function, add(var(x), var(x)).into_pattern(), x),
        Some(5)
    );
}

// ── when_match × commutative swap retry ──────────────────────────────────────

/// A `when_match` guard on a CHILD operand that rejects the natural
/// operand order does not kill the match: the commutative swap retry
/// re-drives the child against the other operand, where the guard
/// passes.
#[test]
fn child_when_match_rejection_still_tries_swapped_order() {
    use strider_ir::IRViewer;

    // add(2, 3): the guarded child sits on pattern slot 0, which the
    // natural order maps to operand 2 (guard fails) and the swap retry
    // maps to operand 3 (guard passes).
    let function = shapes::int_bin(2, 3, IntBinaryOp::Add);
    let c = Capture::new();
    let guarded = any().capture(c).when_match(move |m, _ty, b| {
        let Some(v) = b.get_value(c) else {
            return false;
        };
        m.function().int_const_u128(v) == Some(3)
    });
    let m = a::unique(&function, add(guarded, int_const(2u128)).into_pattern());
    assert_eq!(
        m.bindings().get_uint(c, &function),
        Some(3),
        "swap retry must rebind the guarded child to the 3-operand",
    );
}

/// A `when_match` guard on the ROOT runs after the inputs already
/// resolved in SOME order; if it rejects, the match unwinds entirely —
/// the matcher does NOT re-drive the swapped operand order to satisfy a
/// root guard (pins the documented post-match contract).
#[test]
fn root_when_match_rejection_does_not_redrive_swap() {
    use strider_ir::IRViewer;

    let function = shapes::int_bin(2, 3, IntBinaryOp::Add);
    let l = Capture::new();
    // Inputs match in the natural order (l ← 2); the root guard then
    // demands l == 3, which only the swapped order would satisfy.
    let pat = add(any().capture(l), any())
        .when_match(move |m, _ty, b| {
            let Some(v) = b.get_value(l) else {
                return false;
            };
            m.function().int_const_u128(v) == Some(3)
        })
        .into_pattern();
    a::none(&function, pat);
}

// ── float_cmp commutativity ──────────────────────────────────────────────────

/// Builds a graph that asserts a float comparison `a OP b` and returns
/// the boolean result (cast to u64 for typability).
fn graph_float_cmp(l: f64, r: f64, op: FloatCmpOp) -> strider_ir::Function {
    let mut t = Tb::empty();
    let a = t.f64(l);
    let b = t.f64(r);
    let v = t.fcmp(a, b, op);
    let as_int = t.as_int(v, strider_ir::node::ValueType::I64);
    t.ret_val(as_int)
}

#[test]
fn float_eq_commutes() {
    let function = graph_float_cmp(1.0, 2.0, FloatCmpOp::Equal);
    a::matches_both_orders(
        &function,
        float_cmp(
            FloatCmpOp::Equal,
            float_const(1.0_f64.to_bits()),
            float_const(2.0_f64.to_bits()),
        )
        .into_pattern(),
        float_cmp(
            FloatCmpOp::Equal,
            float_const(2.0_f64.to_bits()),
            float_const(1.0_f64.to_bits()),
        )
        .into_pattern(),
    );
}

#[test]
fn float_ne_commutes() {
    // `FloatCmpOp::NotEqual` is no longer a primitive — `pattern::float_ne`
    // is an ergonomic alias that constructs `Xor(FloatEqual(_, _), 1):I1`.
    let function = {
        let mut t = Tb::empty();
        let a = t.f64(1.0);
        let b = t.f64(2.0);
        let eq = t.fcmp(a, b, FloatCmpOp::Equal);
        let ne = t.bool_not(eq);
        let as_int = t.as_int(ne, strider_ir::node::ValueType::I64);
        t.ret_val(as_int)
    };
    a::matches_both_orders(
        &function,
        float_ne(
            float_const(1.0_f64.to_bits()),
            float_const(2.0_f64.to_bits()),
        )
        .into_pattern(),
        float_ne(
            float_const(2.0_f64.to_bits()),
            float_const(1.0_f64.to_bits()),
        )
        .into_pattern(),
    );
}

#[test]
fn float_lt_does_not_commute() {
    let function = graph_float_cmp(1.0, 2.0, FloatCmpOp::Less);
    // Canonical order matches.
    a::matches(
        &function,
        float_cmp(
            FloatCmpOp::Less,
            float_const(1.0_f64.to_bits()),
            float_const(2.0_f64.to_bits()),
        )
        .into_pattern(),
        1,
    );
    // Swapped order must NOT match — Less is directional.
    a::none(
        &function,
        float_cmp(
            FloatCmpOp::Less,
            float_const(2.0_f64.to_bits()),
            float_const(1.0_f64.to_bits()),
        )
        .into_pattern(),
    );
}
