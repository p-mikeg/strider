//! Variant-agnostic `*_any` constructors.
//!
//! Each `*_any` constructor matches any variant in its op family and binds
//! the actual operator to a typed op-capture variable (`IntBinaryOpVar` etc.).
//! These tests verify:
//!   * every op family has a working `*_any` ctor;
//!   * the matched op variant is retrievable via `match.get_*_op`;
//!   * `*_any` still honours family membership (int_binary_any won't match
//!     a bool op);
//!   * value captures (`.capture(Var)`) compose with op-variant capture.

use ir::node::NodeOutputType;
use ir::{
    BoolBinaryOp, BoolUnaryOp, FloatBinaryOp, FloatCmpOp, FloatUnaryOp, IntBinaryOp, IntCmpOp,
    IntUnaryOp,
};
use pattern::*;

use super::support::{Tb, assertions as a};

// ── Int binary ────────────────────────────────────────────────────────────────

#[test]
fn int_binary_any_captures_each_variant() {
    for op in [
        IntBinaryOp::Add,
        IntBinaryOp::Sub,
        IntBinaryOp::Mul,
        IntBinaryOp::Xor,
        IntBinaryOp::ShiftLeft,
    ] {
        let mut t = Tb::empty();
        let l = t.u64(5);
        let r = t.u64(3);
        let v = t.int_bin(l, r, op);
        let g = t.ret_val(v);

        let ov = IntBinaryOpVar::new();
        let m = a::unique(&g, int_binary_any(ov, int_const(5), int_const(3)));
        assert_eq!(m.get_int_binary_op(ov), Some(op));
    }
}

#[test]
fn int_binary_any_retries_swap_only_for_commutative() {
    // Commutative ops: swap should match.
    let mut t = Tb::empty();
    let l = t.u64(5);
    let r = t.u64(3);
    let v = t.add(l, r);
    let g = t.ret_val(v);
    let ov = IntBinaryOpVar::new();
    a::matches(&g, int_binary_any(ov, int_const(3), int_const(5)), 1);

    // Non-commutative op: swap must NOT match.
    let mut t = Tb::empty();
    let l = t.u64(5);
    let r = t.u64(3);
    let v = t.sub(l, r);
    let g = t.ret_val(v);
    let ov = IntBinaryOpVar::new();
    a::none(&g, int_binary_any(ov, int_const(3), int_const(5)));
}

// ── Int unary ────────────────────────────────────────────────────────────────

#[test]
fn int_unary_any_captures_variant() {
    for op in [IntUnaryOp::Neg, IntUnaryOp::Not] {
        let mut t = Tb::empty();
        let v = t.u64(42);
        let v = t.int_un(v, op);
        let g = t.ret_val(v);

        let ov = IntUnaryOpVar::new();
        let m = a::unique(&g, int_unary_any(ov, int_const(42)));
        assert_eq!(m.get_int_unary_op(ov), Some(op));
    }
}

// ── Int comparison ───────────────────────────────────────────────────────────

#[test]
fn int_cmp_any_captures_variant() {
    for op in [
        IntCmpOp::Equal,
        IntCmpOp::Less,
        IntCmpOp::Sless,
        IntCmpOp::Carry,
    ] {
        let mut t = Tb::empty();
        let l = t.u64(5);
        let r = t.u64(3);
        let c = t.int_cmp(l, r, op);
        let cast = t.as_int(c, NodeOutputType::U64);
        let g = t.ret_val(cast);

        let ov = IntCmpOpVar::new();
        let m = a::unique(&g, int_cmp_any(ov, int_const(5), int_const(3)));
        assert_eq!(m.get_int_cmp_op(ov), Some(op));
    }
}

#[test]
fn int_cmp_any_retries_swap_only_for_commutative_cmp() {
    // Equal is commutative → swap matches.
    let mut t = Tb::empty();
    let l = t.u64(5);
    let r = t.u64(3);
    let c = t.int_cmp(l, r, IntCmpOp::Equal);
    let cast = t.as_int(c, NodeOutputType::U64);
    let g = t.ret_val(cast);
    let ov = IntCmpOpVar::new();
    a::matches(&g, int_cmp_any(ov, int_const(3), int_const(5)), 1);

    // Less is NOT commutative → swap rejects.
    let mut t = Tb::empty();
    let l = t.u64(5);
    let r = t.u64(3);
    let c = t.int_cmp(l, r, IntCmpOp::Less);
    let cast = t.as_int(c, NodeOutputType::U64);
    let g = t.ret_val(cast);
    let ov = IntCmpOpVar::new();
    a::none(&g, int_cmp_any(ov, int_const(3), int_const(5)));
}

#[test]
fn float_cmp_any_does_not_retry_swap() {
    // `float_cmp_any` uses fixed_ordered — no commutativity retry at all.
    let mut t = Tb::empty();
    let l = t.f64(1.0);
    let r = t.f64(2.0);
    let c = t.fcmp(l, r, FloatCmpOp::Equal);
    let cast = t.as_int(c, NodeOutputType::U64);
    let g = t.ret_val(cast);

    let ov = FloatCmpOpVar::new();
    a::none(
        &g,
        float_cmp_any(ov, float_const(2.0f64.to_bits()), float_const(1.0f64.to_bits())),
    );
}

// ── Bool binary / unary ──────────────────────────────────────────────────────

#[test]
fn bool_binary_any_captures_variant() {
    for op in [BoolBinaryOp::And, BoolBinaryOp::Or, BoolBinaryOp::Xor] {
        let mut t = Tb::empty();
        let a_ = t.boolean(true);
        let b_ = t.boolean(false);
        let c = t.bool_bin(a_, b_, op);
        let cast = t.as_int(c, NodeOutputType::U64);
        let g = t.ret_val(cast);

        let ov = BoolBinaryOpVar::new();
        let m = a::unique(&g, bool_binary_any(ov, bool_const(true), bool_const(false)));
        assert_eq!(m.get_bool_binary_op(ov), Some(op));
    }
}

#[test]
fn bool_unary_any_captures_variant() {
    let mut t = Tb::empty();
    let v = t.boolean(true);
    let v = t.bool_un(v, BoolUnaryOp::Neg);
    let cast = t.as_int(v, NodeOutputType::U64);
    let g = t.ret_val(cast);

    let ov = BoolUnaryOpVar::new();
    let m = a::unique(&g, bool_unary_any(ov, bool_const(true)));
    assert_eq!(m.get_bool_unary_op(ov), Some(BoolUnaryOp::Neg));
}

// ── Float binary / unary / cmp ───────────────────────────────────────────────

#[test]
fn float_binary_any_captures_variant() {
    for op in [
        FloatBinaryOp::Add,
        FloatBinaryOp::Sub,
        FloatBinaryOp::Mul,
        FloatBinaryOp::Div,
    ] {
        let mut t = Tb::empty();
        let l = t.f64(1.0);
        let r = t.f64(2.0);
        let v = t.fbin(l, r, op, NodeOutputType::F64);
        let cast = t.float_to_int(v, NodeOutputType::U64);
        let g = t.ret_val(cast);

        let ov = FloatBinaryOpVar::new();
        let m = a::unique(
            &g,
            float_binary_any(ov, float_const(1.0f64.to_bits()), float_const(2.0f64.to_bits())),
        );
        assert_eq!(m.get_float_binary_op(ov), Some(op));
    }
}

#[test]
fn float_unary_any_captures_variant() {
    for op in [
        FloatUnaryOp::Neg,
        FloatUnaryOp::Abs,
        FloatUnaryOp::Sqrt,
        FloatUnaryOp::Ceil,
    ] {
        let mut t = Tb::empty();
        let v = t.f64(9.0);
        let v = t.fun(v, op, NodeOutputType::F64);
        let cast = t.float_to_int(v, NodeOutputType::U64);
        let g = t.ret_val(cast);

        let ov = FloatUnaryOpVar::new();
        let m = a::unique(&g, float_unary_any(ov, float_const(9.0f64.to_bits())));
        assert_eq!(m.get_float_unary_op(ov), Some(op));
    }
}

#[test]
fn float_cmp_any_captures_variant() {
    for op in [
        FloatCmpOp::Equal,
        FloatCmpOp::NotEqual,
        FloatCmpOp::Less,
        FloatCmpOp::LessEqual,
    ] {
        let mut t = Tb::empty();
        let l = t.f64(1.0);
        let r = t.f64(2.0);
        let c = t.fcmp(l, r, op);
        let cast = t.as_int(c, NodeOutputType::U64);
        let g = t.ret_val(cast);

        let ov = FloatCmpOpVar::new();
        let m = a::unique(
            &g,
            float_cmp_any(ov, float_const(1.0f64.to_bits()), float_const(2.0f64.to_bits())),
        );
        assert_eq!(m.get_float_cmp_op(ov), Some(op));
    }
}

// ── Cross-family rejection ───────────────────────────────────────────────────

#[test]
fn int_binary_any_does_not_match_bool_op() {
    // Graph has a BoolBinaryOp::Or; int_binary_any must not match.
    let mut t = Tb::empty();
    let a_ = t.boolean(true);
    let b_ = t.boolean(false);
    let c = t.bool_bin(a_, b_, BoolBinaryOp::Or);
    let cast = t.as_int(c, NodeOutputType::U64);
    let g = t.ret_val(cast);

    let ov = IntBinaryOpVar::new();
    a::none(&g, int_binary_any(ov, any(), any()));
}

// ── Op-variant capture combined with value capture ───────────────────────────

#[test]
fn variant_any_composes_with_value_capture() {
    let mut t = Tb::empty();
    let l = t.u64(100);
    let r = t.u64(50);
    let v = t.sub(l, r);
    let g = t.ret_val(v);

    let ov = IntBinaryOpVar::new();
    let lv = IntVar::new();
    let rv = IntVar::new();
    let m = a::unique(&g, int_binary_any(ov, any_int_const(lv), any_int_const(rv)));

    assert_eq!(m.get_int_binary_op(ov), Some(IntBinaryOp::Sub));
    assert_eq!(m.get_int(lv), Some(100));
    assert_eq!(m.get_int(rv), Some(50));
}

#[test]
fn unbound_op_var_returns_none() {
    // If a pattern doesn't use an IntBinaryOpVar, `.get_int_binary_op` on it
    // returns None.
    let g = {
        let mut t = Tb::empty();
        let v = t.u64(7);
        t.ret_val(v)
    };
    let m = a::first(&g, int_const(7));
    let ov = IntBinaryOpVar::new();
    assert_eq!(m.get_int_binary_op(ov), None);
}
