//! Variant-agnostic `*_any` constructors: match any variant in an op family,
//! bind the node to a [`Capture`], then recover the variant afterwards via
//! `Match::get_*_op(c, &graph)`.

use strider_ir::node::ValueType;
use strider_ir::{
    FloatBinaryOp, FloatCmpOp, FloatUnaryOp, IRViewer, IntBinaryOp, IntCmpOp, IntUnaryOp,
};
use strider_pattern::*;

use super::support::{Tb, assertions as a};

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
        let function = t.ret_val(v);

        let ov = Capture::new();
        let m = a::unique(
            &function,
            int_binary_any(int_const(5u128), int_const(3u128))
                .capture(ov)
                .into_pattern(),
        );
        assert_eq!(
            m.bindings().get_int_binary_op(ov, function.graph()),
            Some(op)
        );
    }
}

#[test]
fn int_binary_any_retries_swap_only_for_commutative() {
    // Add is commutative, so the swapped pattern still matches.
    let mut t = Tb::empty();
    let l = t.u64(5);
    let r = t.u64(3);
    let v = t.add(l, r);
    let function = t.ret_val(v);
    let ov = Capture::new();
    a::matches(
        &function,
        int_binary_any(int_const(3u128), int_const(5u128))
            .capture(ov)
            .into_pattern(),
        1,
    );

    // Sub is not, so it must not.
    let mut t = Tb::empty();
    let l = t.u64(5);
    let r = t.u64(3);
    let v = t.sub(l, r);
    let function = t.ret_val(v);
    let ov = Capture::new();
    a::none(
        &function,
        int_binary_any(int_const(3u128), int_const(5u128))
            .capture(ov)
            .into_pattern(),
    );
}

#[test]
fn int_unary_any_captures_variant() {
    // Neg is the only IntUnaryOp; complement is Xor(x, all_ones).
    let op = IntUnaryOp::Neg;
    let mut t = Tb::empty();
    let v = t.u64(42);
    let v = t.int_un(v, op);
    let function = t.ret_val(v);

    let ov = Capture::new();
    let m = a::unique(
        &function,
        int_unary_any(int_const(42u128)).capture(ov).into_pattern(),
    );
    assert_eq!(
        m.bindings().get_int_unary_op(ov, function.graph()),
        Some(op)
    );
}

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
        let cast = t.as_int(c, ValueType::I64);
        let function = t.ret_val(cast);

        let ov = Capture::new();
        let m = a::unique(
            &function,
            int_cmp_any(int_const(5u128), int_const(3u128))
                .capture(ov)
                .into_pattern(),
        );
        assert_eq!(m.bindings().get_int_cmp_op(ov, function.graph()), Some(op));
    }
}

#[test]
fn int_cmp_any_retries_swap_only_for_commutative_cmp() {
    // Equal is symmetric.
    let mut t = Tb::empty();
    let l = t.u64(5);
    let r = t.u64(3);
    let c = t.int_cmp(l, r, IntCmpOp::Equal);
    let cast = t.as_int(c, ValueType::I64);
    let function = t.ret_val(cast);
    let ov = Capture::new();
    a::matches(
        &function,
        int_cmp_any(int_const(3u128), int_const(5u128))
            .capture(ov)
            .into_pattern(),
        1,
    );

    // Less is directional.
    let mut t = Tb::empty();
    let l = t.u64(5);
    let r = t.u64(3);
    let c = t.int_cmp(l, r, IntCmpOp::Less);
    let cast = t.as_int(c, ValueType::I64);
    let function = t.ret_val(cast);
    let ov = Capture::new();
    a::none(
        &function,
        int_cmp_any(int_const(3u128), int_const(5u128))
            .capture(ov)
            .into_pattern(),
    );
}

#[test]
fn float_cmp_any_retries_swap_only_for_commutative_cmp() {
    // Equal is symmetric.
    let mut t = Tb::empty();
    let l = t.f64(1.0);
    let r = t.f64(2.0);
    let c = t.fcmp(l, r, FloatCmpOp::Equal);
    let cast = t.as_int(c, ValueType::I64);
    let function = t.ret_val(cast);

    let ov = Capture::new();
    a::matches(
        &function,
        float_cmp_any(float_const(2.0f64.to_bits()), float_const(1.0f64.to_bits()))
            .capture(ov)
            .into_pattern(),
        1,
    );

    // Less is directional.
    let mut t2 = Tb::empty();
    let l2 = t2.f64(1.0);
    let r2 = t2.f64(2.0);
    let c2 = t2.fcmp(l2, r2, FloatCmpOp::Less);
    let cast2 = t2.as_int(c2, ValueType::I64);
    let g2 = t2.ret_val(cast2);
    let ov2 = Capture::new();
    a::none(
        &g2,
        float_cmp_any(float_const(2.0f64.to_bits()), float_const(1.0f64.to_bits()))
            .capture(ov2)
            .into_pattern(),
    );
}

#[test]
fn bool_bin_any_captures_variant() {
    // Booleans are 1-bit ints, so a boolean binary op is an IntBinaryOp at I1.
    for op in [IntBinaryOp::And, IntBinaryOp::Or, IntBinaryOp::Xor] {
        let mut t = Tb::empty();
        let a_ = t.boolean(true);
        let b_ = t.boolean(false);
        let c = t.bool_bin(a_, b_, op);
        let cast = t.as_int(c, ValueType::I64);
        let function = t.ret_val(cast);

        let ov = Capture::new();
        let m = a::unique(
            &function,
            bool_bin_any(bool_const(true), bool_const(false))
                .capture(ov)
                .into_pattern(),
        );
        assert_eq!(
            m.bindings().get_bool_binary_op(ov, function.graph()),
            Some(op)
        );
    }
}

// There are no bool-unary tests: with BitNot gone, a "bool unary op" is
// Xor(_, IntConst(1)):I1, covered by bool_bin_any with an all-ones operand.

/// After the bool-to-I1 collapse a wide 64-bit `And` shares its `NodeKind`
/// with a boolean one, so `bool_bin_any` must gate on the `I1` output and
/// reject the wide op.
#[test]
fn bool_bin_any_rejects_wide_int_op() {
    let mut t = Tb::empty();
    let bt = t.boolean(true);
    let bf = t.boolean(false);
    let bool_and = t.bool_bin(bt, bf, IntBinaryOp::And);
    let bool_as_int = t.as_int(bool_and, ValueType::I64);
    // Same NodeKind discriminant and payload, but 64-bit.
    let w0 = t.u64(0xF0);
    let w1 = t.u64(0x0F);
    let wide_and = t.int_bin(w0, w1, IntBinaryOp::And);
    let sum = t.add(bool_as_int, wide_and);
    let function = t.ret_val(sum);

    let ob = Capture::new();
    let hits = a::matches(
        &function,
        bool_bin_any(any(), any()).capture(ob).into_pattern(),
        1,
    );
    let value = hits[0].value(ob).expect("matched value output");
    assert_eq!(
        function
            .value_kind(value)
            .as_value()
            .map(|ty| ty.bit_width()),
        Some(1),
        "bool_bin_any must match only the I1-output op, not a wide one",
    );
}

#[test]
fn float_binary_any_captures_variant() {
    for op in [FloatBinaryOp::Add, FloatBinaryOp::Mul, FloatBinaryOp::Div] {
        let mut t = Tb::empty();
        let l = t.f64(1.0);
        let r = t.f64(2.0);
        let v = t.fbin(l, r, op, ValueType::F64);
        let cast = t.float_to_int(v, ValueType::I64);
        let function = t.ret_val(cast);

        let ov = Capture::new();
        let m = a::unique(
            &function,
            float_binary_any(float_const(1.0f64.to_bits()), float_const(2.0f64.to_bits()))
                .capture(ov)
                .into_pattern(),
        );
        assert_eq!(
            m.bindings().get_float_binary_op(ov, function.graph()),
            Some(op)
        );
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
        let v = t.fun(v, op, ValueType::F64);
        let cast = t.float_to_int(v, ValueType::I64);
        let function = t.ret_val(cast);

        let ov = Capture::new();
        let m = a::unique(
            &function,
            float_unary_any(float_const(9.0f64.to_bits()))
                .capture(ov)
                .into_pattern(),
        );
        assert_eq!(
            m.bindings().get_float_unary_op(ov, function.graph()),
            Some(op)
        );
    }
}

#[test]
fn float_cmp_any_captures_variant() {
    for op in [FloatCmpOp::Equal, FloatCmpOp::Less] {
        let mut t = Tb::empty();
        let l = t.f64(1.0);
        let r = t.f64(2.0);
        let c = t.fcmp(l, r, op);
        let cast = t.as_int(c, ValueType::I64);
        let function = t.ret_val(cast);

        let ov = Capture::new();
        let m = a::unique(
            &function,
            float_cmp_any(float_const(1.0f64.to_bits()), float_const(2.0f64.to_bits()))
                .capture(ov)
                .into_pattern(),
        );
        assert_eq!(
            m.bindings().get_float_cmp_op(ov, function.graph()),
            Some(op)
        );
    }
}

#[test]
fn variant_any_composes_with_value_capture() {
    let mut t = Tb::empty();
    let l = t.u64(100);
    let r = t.u64(50);
    let v = t.mul(l, r);
    let function = t.ret_val(v);

    let ov = Capture::new();
    let lv = Capture::new();
    let rv = Capture::new();
    // Mul is commutative and both captures sit on operands, so the two operand
    // orderings are two distinct bindings, natural order first.
    let hits = a::matches(
        &function,
        int_binary_any(any_int_const().capture(lv), any_int_const().capture(rv))
            .capture(ov)
            .into_pattern(),
        2,
    );

    for m in &hits {
        assert_eq!(
            m.bindings().get_int_binary_op(ov, function.graph()),
            Some(IntBinaryOp::Mul),
            "the op-variant capture composes with the value captures either way",
        );
    }
    assert_eq!(hits[0].bindings().get_uint(lv, &function), Some(100));
    assert_eq!(hits[0].bindings().get_uint(rv, &function), Some(50));
    assert_eq!(hits[1].bindings().get_uint(lv, &function), Some(50));
    assert_eq!(hits[1].bindings().get_uint(rv, &function), Some(100));
}

#[test]
fn unbound_op_capture_returns_none() {
    let function = {
        let mut t = Tb::empty();
        let v = t.u64(7);
        t.ret_val(v)
    };
    let m = a::first(&function, int_const(7u128).into_pattern());
    let ov = Capture::new();
    assert_eq!(m.bindings().get_int_binary_op(ov, function.graph()), None);
}
