//! Variant-agnostic `*_any` constructors.
//!
//! Each `*_any` constructor matches any variant in its op family and binds
//! the matched node to a [`Capture`].  The op variant is recovered after
//! the match via the matching `Match::get_*_op(c, &graph)` helper.

use strider_analyze::pattern::*;
use strider_ir::node::NodeOutputType;
use strider_ir::{
    BoolBinaryOp, BoolUnaryOp, FloatBinaryOp, FloatCmpOp, FloatUnaryOp, IntBinaryOp, IntCmpOp,
    IntUnaryOp,
};

use super::support::{Tb, assertions as a};

// ── Int binary ────────────────────────────────────────────────────────────────

#[test]
fn int_binary_any_captures_each_variant() {
    for op in [
        IntBinaryOp::Add,
        IntBinaryOp::Mul,
        IntBinaryOp::Xor,
        IntBinaryOp::ShiftLeft,
    ] {
        let mut t = Tb::empty();
        let l = t.u64(5);
        let r = t.u64(3);
        let v = t.int_bin(l, r, op);
        let g = t.ret_val(v);

        let ov = Capture::new();
        let m = a::unique(&g, int_binary_any(ov, int_const(5), int_const(3)));
        assert_eq!(m.get_int_binary_op(ov, &g), Some(op));
    }
}

#[test]
fn int_binary_any_retries_swap_only_for_commutative() {
    // Commutative: swap should match.
    let mut t = Tb::empty();
    let l = t.u64(5);
    let r = t.u64(3);
    let v = t.add(l, r);
    let g = t.ret_val(v);
    let ov = Capture::new();
    a::matches(&g, int_binary_any(ov, int_const(3), int_const(5)), 1);

    // Non-commutative: swap must NOT match.
    let mut t = Tb::empty();
    let l = t.u64(5);
    let r = t.u64(3);
    let v = t.sub(l, r);
    let g = t.ret_val(v);
    let ov = Capture::new();
    a::none(&g, int_binary_any(ov, int_const(3), int_const(5)));
}

// ── Int unary ────────────────────────────────────────────────────────────────

#[test]
fn int_unary_any_captures_variant() {
    for op in [IntUnaryOp::BitNot, IntUnaryOp::Neg] {
        let mut t = Tb::empty();
        let v = t.u64(42);
        let v = t.int_un(v, op);
        let g = t.ret_val(v);

        let ov = Capture::new();
        let m = a::unique(&g, int_unary_any(ov, int_const(42)));
        assert_eq!(m.get_int_unary_op(ov, &g), Some(op));
    }
}

// ── Int comparison ───────────────────────────────────────────────────────────

#[test]
fn int_cmp_any_captures_variant() {
    for op in [IntCmpOp::Equal, IntCmpOp::Less, IntCmpOp::Sless, IntCmpOp::Carry] {
        let mut t = Tb::empty();
        let l = t.u64(5);
        let r = t.u64(3);
        let c = t.int_cmp(l, r, op);
        let cast = t.as_int(c, NodeOutputType::U64);
        let g = t.ret_val(cast);

        let ov = Capture::new();
        let m = a::unique(&g, int_cmp_any(ov, int_const(5), int_const(3)));
        assert_eq!(m.get_int_cmp_op(ov, &g), Some(op));
    }
}

#[test]
fn int_cmp_any_retries_swap_only_for_commutative_cmp() {
    // Equal commutative → swap matches.
    let mut t = Tb::empty();
    let l = t.u64(5);
    let r = t.u64(3);
    let c = t.int_cmp(l, r, IntCmpOp::Equal);
    let cast = t.as_int(c, NodeOutputType::U64);
    let g = t.ret_val(cast);
    let ov = Capture::new();
    a::matches(&g, int_cmp_any(ov, int_const(3), int_const(5)), 1);

    // Less NOT commutative → swap rejects.
    let mut t = Tb::empty();
    let l = t.u64(5);
    let r = t.u64(3);
    let c = t.int_cmp(l, r, IntCmpOp::Less);
    let cast = t.as_int(c, NodeOutputType::U64);
    let g = t.ret_val(cast);
    let ov = Capture::new();
    a::none(&g, int_cmp_any(ov, int_const(3), int_const(5)));
}

#[test]
fn float_cmp_any_retries_swap_only_for_commutative_cmp() {
    // Equal is symmetric.
    let mut t = Tb::empty();
    let l = t.f64(1.0);
    let r = t.f64(2.0);
    let c = t.fcmp(l, r, FloatCmpOp::Equal);
    let cast = t.as_int(c, NodeOutputType::U64);
    let g = t.ret_val(cast);

    let ov = Capture::new();
    a::matches(
        &g,
        float_cmp_any(ov, float_const(2.0f64.to_bits()), float_const(1.0f64.to_bits())),
        1,
    );

    // Less is directional.
    let mut t2 = Tb::empty();
    let l2 = t2.f64(1.0);
    let r2 = t2.f64(2.0);
    let c2 = t2.fcmp(l2, r2, FloatCmpOp::Less);
    let cast2 = t2.as_int(c2, NodeOutputType::U64);
    let g2 = t2.ret_val(cast2);
    let ov2 = Capture::new();
    a::none(
        &g2,
        float_cmp_any(ov2, float_const(2.0f64.to_bits()), float_const(1.0f64.to_bits())),
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

        let ov = Capture::new();
        let m = a::unique(&g, bool_binary_any(ov, bool_const(true), bool_const(false)));
        assert_eq!(m.get_bool_binary_op(ov, &g), Some(op));
    }
}

#[test]
fn bool_unary_any_captures_variant() {
    let op = BoolUnaryOp::Neg;
    let mut t = Tb::empty();
    let v = t.boolean(true);
    let v = t.bool_un(v, op);
    let cast = t.as_int(v, NodeOutputType::U64);
    let g = t.ret_val(cast);

    let ov = Capture::new();
    let m = a::unique(&g, bool_unary_any(ov, bool_const(true)));
    assert_eq!(m.get_bool_unary_op(ov, &g), Some(op));
}

// ── Float binary / unary / cmp ───────────────────────────────────────────────

#[test]
fn float_binary_any_captures_variant() {
    for op in [FloatBinaryOp::Add, FloatBinaryOp::Mul, FloatBinaryOp::Div] {
        let mut t = Tb::empty();
        let l = t.f64(1.0);
        let r = t.f64(2.0);
        let v = t.fbin(l, r, op, NodeOutputType::F64);
        let cast = t.float_to_int(v, NodeOutputType::U64);
        let g = t.ret_val(cast);

        let ov = Capture::new();
        let m = a::unique(
            &g,
            float_binary_any(ov, float_const(1.0f64.to_bits()), float_const(2.0f64.to_bits())),
        );
        assert_eq!(m.get_float_binary_op(ov, &g), Some(op));
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

        let ov = Capture::new();
        let m = a::unique(&g, float_unary_any(ov, float_const(9.0f64.to_bits())));
        assert_eq!(m.get_float_unary_op(ov, &g), Some(op));
    }
}

#[test]
fn float_cmp_any_captures_variant() {
    for op in [FloatCmpOp::Equal, FloatCmpOp::Less] {
        let mut t = Tb::empty();
        let l = t.f64(1.0);
        let r = t.f64(2.0);
        let c = t.fcmp(l, r, op);
        let cast = t.as_int(c, NodeOutputType::U64);
        let g = t.ret_val(cast);

        let ov = Capture::new();
        let m = a::unique(
            &g,
            float_cmp_any(ov, float_const(1.0f64.to_bits()), float_const(2.0f64.to_bits())),
        );
        assert_eq!(m.get_float_cmp_op(ov, &g), Some(op));
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

    let ov = Capture::new();
    a::none(&g, int_binary_any(ov, any(), any()));
}

// ── Op-variant capture combined with value capture ───────────────────────────

#[test]
fn variant_any_composes_with_value_capture() {
    let mut t = Tb::empty();
    let l = t.u64(100);
    let r = t.u64(50);
    let v = t.mul(l, r);
    let g = t.ret_val(v);

    let ov = Capture::new();
    let lv = Capture::new();
    let rv = Capture::new();
    let m = a::unique(&g, int_binary_any(ov, any_int_const(lv), any_int_const(rv)));

    assert_eq!(m.get_int_binary_op(ov, &g), Some(IntBinaryOp::Mul));
    assert_eq!(m.get_uint(lv, &g), Some(100));
    assert_eq!(m.get_uint(rv, &g), Some(50));
}

#[test]
fn unbound_op_capture_returns_none() {
    let g = {
        let mut t = Tb::empty();
        let v = t.u64(7);
        t.ret_val(v)
    };
    let m = a::first(&g, int_const(7));
    let ov = Capture::new();
    assert_eq!(m.get_int_binary_op(ov, &g), None);
}
