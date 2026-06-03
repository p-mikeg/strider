//! Integration coverage for the compile-time-typed `MatchPat` builder
//! family. Each test builds a small IR fixture and asserts the typed
//! pattern matches exactly once (and extracts captures where relevant).

#![allow(
    clippy::panic,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::unreachable
)]

use strider_ir::node::ValueType as T;
use strider_ir::{ExtendOp, FloatBinaryOp, FloatCmpOp, FloatUnaryOp, IntBinaryOp, IntCmpOp};
use strider_ir_test_utils::{make_empty_fn, reg_vn};

use strider_pattern::{
    CaptureExt, Capture, MatchPat, Matcher, add, and, any, any_bool_const, any_float_const,
    any_int_const, bit_not, bool_and, bool_binary, bool_const, bool_inputs, bool_not, bool_or,
    bool_value, bool_xor, div, extend, float_abs, float_add, float_binary, float_binary_any,
    float_ceil, float_cmp, float_cmp_any, float_const, float_div, float_eq, float_floor,
    float_is_nan, float_le, float_lt, float_mul, float_ne, float_neg, float_round, float_sqrt,
    float_sub, float_to_int, initial_var, initial_var_for, inputs_of_width, int_binary,
    int_binary_any, int_carry, int_cmp, int_cmp_any, int_const,
    int_const_any_of, int_eq, int_le, int_lt, int_ne, int_sborrow, int_scarry, int_sle, int_slt,
    int_unary_any, lzcount, mul, neg, not_, or, popcount, predicate, rem, sdiv, shl, shr,
    sign_extend, signed_int_const, srem, sshr, sub, truncate, value_of_width, var, xor,
    zero_extend,
};

/// Build the pattern from `f` and count its matches against `fixture`.
fn count(f: impl Fn() -> strider_pattern::pattern::Pattern, fixture: &strider_ir::Function) -> usize {
    let pat = f();
    Matcher::try_new(fixture).unwrap().find_all(&pat).unwrap().len()
}

// ── Task 3.1 core: Add / Var / Any / IntConst ─────────────────────────

#[test]
fn typed_add_matches_and_captures() {
    let fx = make_empty_fn(|b| {
        let x = b.build_int_const(5u64, T::I64)?;
        let k = b.build_int_const(1u64, T::I64)?;
        b.build_int_binary_operation(x, k, IntBinaryOp::Add, T::I64)
    })
    .unwrap();
    let c = Capture::new();
    let pat = add(var(c), int_const(1u128)).into_pattern();
    let hits = Matcher::try_new(&fx).unwrap().find_all(&pat).unwrap();
    assert_eq!(hits.len(), 1);
    assert!(hits[0].value(c).is_some());
    // any matches every node; int_const(5) hits exactly the 5 const.
    assert_eq!(count(|| any().into_pattern(), &fx), node_count(&fx));
    assert_eq!(count(|| int_const(5u128).into_pattern(), &fx), 1);
}

// ── Integer binary ops ────────────────────────────────────────────────

#[test]
fn int_binary_family() {
    let v = reg_vn(0, 8);
    macro_rules! bin {
        ($build:ident, $pat:expr) => {{
            let fx = strider_ir_test_utils::make_fn_with_var(v, |b, base| {
                let k = b.build_int_const(3u64, T::I64)?;
                b.build_int_binary_operation(base, k, IntBinaryOp::$build, T::I64)
            })
            .unwrap()
            .0;
            assert_eq!(count(|| $pat, &fx), 1, stringify!($build));
        }};
    }
    bin!(Mul, mul(var(Capture::new()), int_const(3u128)).into_pattern());
    bin!(Div, div(var(Capture::new()), int_const(3u128)).into_pattern());
    bin!(Sdiv, sdiv(var(Capture::new()), int_const(3u128)).into_pattern());
    bin!(Rem, rem(var(Capture::new()), int_const(3u128)).into_pattern());
    bin!(Srem, srem(var(Capture::new()), int_const(3u128)).into_pattern());
    bin!(And, and(var(Capture::new()), int_const(3u128)).into_pattern());
    bin!(Or, or(var(Capture::new()), int_const(3u128)).into_pattern());
    bin!(Xor, xor(var(Capture::new()), int_const(3u128)).into_pattern());
    bin!(ShiftLeft, shl(var(Capture::new()), int_const(3u128)).into_pattern());
    bin!(ShiftRight, shr(var(Capture::new()), int_const(3u128)).into_pattern());
    bin!(SShiftRight, sshr(var(Capture::new()), int_const(3u128)).into_pattern());
    // int_binary(op, …) runtime variant + int_binary_any.
    bin!(Add, int_binary(IntBinaryOp::Add, var(Capture::new()), int_const(3u128)).into_pattern());
    bin!(Mul, int_binary_any(var(Capture::new()), int_const(3u128)).into_pattern());
}

#[test]
fn sub_lowers_to_add_neg() {
    let v = reg_vn(0, 8);
    let fx = strider_ir_test_utils::make_fn_with_var(v, |b, base| {
        let k = b.build_int_const(7u64, T::I64)?;
        b.build_sub_as_add_neg(base, k, T::I64)
    })
    .unwrap()
    .0;
    assert_eq!(count(|| sub(var(Capture::new()), int_const(7u128)).into_pattern(), &fx), 1);
}

// ── Integer unary ops ─────────────────────────────────────────────────

#[test]
fn int_unary_family() {
    let v = reg_vn(0, 8);
    // neg
    let fx = strider_ir_test_utils::make_fn_with_var(v, |b, base| {
        b.build_int_unary_operation(base, strider_ir::IntUnaryOp::Neg, T::I64)
    })
    .unwrap()
    .0;
    assert_eq!(count(|| neg(var(Capture::new())).into_pattern(), &fx), 1);
    assert_eq!(count(|| int_unary_any(var(Capture::new())).into_pattern(), &fx), 1);

    // popcount
    let fx = strider_ir_test_utils::make_fn_with_var(v, |b, base| b.build_popcount(base, T::I64))
        .unwrap()
        .0;
    assert_eq!(count(|| popcount(var(Capture::new())).into_pattern(), &fx), 1);

    // lzcount
    let fx = strider_ir_test_utils::make_fn_with_var(v, |b, base| b.build_lzcount(base, T::I64))
        .unwrap()
        .0;
    assert_eq!(count(|| lzcount(var(Capture::new())).into_pattern(), &fx), 1);

    // bit_not / not_ : xor(x, all_ones)
    let fx = strider_ir_test_utils::make_fn_with_var(v, |b, base| {
        let ones = b.build_int_const(u128::MAX, T::I64)?;
        b.build_int_binary_operation(base, ones, IntBinaryOp::Xor, T::I64)
    })
    .unwrap()
    .0;
    assert_eq!(count(|| bit_not(var(Capture::new())).into_pattern(), &fx), 1);
    assert_eq!(count(|| not_(var(Capture::new())).into_pattern(), &fx), 1);
}

// ── Casts ─────────────────────────────────────────────────────────────

#[test]
fn cast_family() {
    let v = reg_vn(0, 8);
    // truncate I64 -> I32
    let fx = strider_ir_test_utils::make_fn_with_var(v, |b, base| {
        let t = b.truncate_if_needed(base, T::I32)?;
        b.extend_if_needed(t, T::I64, ExtendOp::ZeroExtend)
    })
    .unwrap()
    .0;
    assert_eq!(count(|| truncate(var(Capture::new())).into_pattern(), &fx), 1);
    assert_eq!(count(|| zero_extend(var(Capture::new())).into_pattern(), &fx), 1);
    assert_eq!(count(|| extend(ExtendOp::ZeroExtend, var(Capture::new())).into_pattern(), &fx), 1);

    // sign_extend
    let fx = strider_ir_test_utils::make_fn_with_var(v, |b, base| {
        let t = b.truncate_if_needed(base, T::I32)?;
        b.extend_if_needed(t, T::I64, ExtendOp::SignExtend)
    })
    .unwrap()
    .0;
    assert_eq!(count(|| sign_extend(var(Capture::new())).into_pattern(), &fx), 1);

    // float_to_int + int_to_float + bitcasts via a chain
    let fx = strider_ir_test_utils::make_fn_with_var(v, |b, base| {
        let f = b.build_int_to_float(base, T::F64)?;
        b.build_float_to_int(f, T::I64)
    })
    .unwrap()
    .0;
    assert_eq!(count(|| float_to_int(any()).into_pattern(), &fx), 1);

    // int_bits_to_float / float_bits_to_int round-trip
    let fx = strider_ir_test_utils::make_fn_with_var(v, |b, base| {
        let f = b.build_int_bits_to_float(base, T::F64)?;
        b.build_float_bits_to_int(f, T::I64)
    })
    .unwrap()
    .0;
    assert_eq!(
        count(|| strider_pattern::int_bits_to_float(any()).into_pattern(), &fx),
        1
    );
    assert_eq!(
        count(|| strider_pattern::float_bits_to_int(any()).into_pattern(), &fx),
        1
    );

    // float_to_float F64 -> F32
    let fx = strider_ir_test_utils::make_fn_with_var(v, |b, base| {
        let f = b.build_int_to_float(base, T::F64)?;
        b.build_float_to_float(f, T::F32)
    })
    .unwrap()
    .0;
    assert_eq!(count(|| strider_pattern::float_to_float(any()).into_pattern(), &fx), 1);
}

// ── Integer comparisons ───────────────────────────────────────────────

#[test]
fn int_cmp_family() {
    let v = reg_vn(0, 8);
    macro_rules! cmp {
        ($op:ident, $pat:expr) => {{
            let fx = strider_ir_test_utils::make_fn_with_var(v, |b, base| {
                let k = b.build_int_const(9u64, T::I64)?;
                let cmp = b.build_int_cmp_operation(base, k, IntCmpOp::$op, T::I64)?;
                b.extend_if_needed(cmp, T::I64, ExtendOp::ZeroExtend)
            })
            .unwrap()
            .0;
            assert_eq!(count(|| $pat, &fx), 1, stringify!($op));
        }};
    }
    cmp!(Equal, int_eq(var(Capture::new()), int_const(9u128)).into_pattern());
    cmp!(Less, int_lt(var(Capture::new()), int_const(9u128)).into_pattern());
    cmp!(Sless, int_slt(var(Capture::new()), int_const(9u128)).into_pattern());
    cmp!(Carry, int_carry(var(Capture::new()), int_const(9u128)).into_pattern());
    cmp!(Scarry, int_scarry(var(Capture::new()), int_const(9u128)).into_pattern());
    cmp!(Sborrow, int_sborrow(var(Capture::new()), int_const(9u128)).into_pattern());
    cmp!(Equal, int_cmp(IntCmpOp::Equal, var(Capture::new()), int_const(9u128)).into_pattern());
    cmp!(Less, int_cmp_any(var(Capture::new()), int_const(9u128)).into_pattern());
}

#[test]
fn int_cmp_lowered_shapes() {
    let v = reg_vn(0, 8);
    // int_ne(a, 9) -> xor(eq(a, 9), 1):I1
    let fx = strider_ir_test_utils::make_fn_with_var(v, |b, base| {
        let k = b.build_int_const(9u64, T::I64)?;
        let eq = b.build_int_cmp_operation(base, k, IntCmpOp::Equal, T::I64)?;
        let one = b.build_int_const(1u64, T::I1)?;
        let ne = b.build_int_binary_operation(eq, one, IntBinaryOp::Xor, T::I1)?;
        b.extend_if_needed(ne, T::I64, ExtendOp::ZeroExtend)
    })
    .unwrap()
    .0;
    assert_eq!(count(|| int_ne(var(Capture::new()), int_const(9u128)).into_pattern(), &fx), 1);

    // int_le(a, 9) -> xor(lt(9, a), 1):I1
    let fx = strider_ir_test_utils::make_fn_with_var(v, |b, base| {
        let k = b.build_int_const(9u64, T::I64)?;
        let lt = b.build_int_cmp_operation(k, base, IntCmpOp::Less, T::I64)?;
        let one = b.build_int_const(1u64, T::I1)?;
        let le = b.build_int_binary_operation(lt, one, IntBinaryOp::Xor, T::I1)?;
        b.extend_if_needed(le, T::I64, ExtendOp::ZeroExtend)
    })
    .unwrap()
    .0;
    assert_eq!(count(|| int_le(var(Capture::new()), int_const(9u128)).into_pattern(), &fx), 1);

    // int_sle(a, 9) -> xor(slt(9, a), 1):I1 (signed operand swap + NOT)
    let fx = strider_ir_test_utils::make_fn_with_var(v, |b, base| {
        let k = b.build_int_const(9u64, T::I64)?;
        let slt = b.build_int_cmp_operation(k, base, IntCmpOp::Sless, T::I64)?;
        let one = b.build_int_const(1u64, T::I1)?;
        let sle = b.build_int_binary_operation(slt, one, IntBinaryOp::Xor, T::I1)?;
        b.extend_if_needed(sle, T::I64, ExtendOp::ZeroExtend)
    })
    .unwrap()
    .0;
    assert_eq!(count(|| int_sle(var(Capture::new()), int_const(9u128)).into_pattern(), &fx), 1);
}

// ── Float ops ─────────────────────────────────────────────────────────

#[test]
fn float_family() {
    let v = reg_vn(0, 8);
    fn fbase(b: &mut strider_ir::FunctionBuilder, base: strider_ir::node::ValueId) -> anyhow::Result<strider_ir::node::ValueId> {
        b.build_int_to_float(base, T::F64)
    }
    // float_add
    let fx = strider_ir_test_utils::make_fn_with_var(v, |b, base| {
        let f = fbase(b, base)?;
        let c = b.build_float_const(0x4000_0000_0000_0000, T::F64);
        let r = b.build_float_binary_op(f, c, FloatBinaryOp::Add, T::F64)?;
        b.build_float_to_int(r, T::I64)
    })
    .unwrap()
    .0;
    assert_eq!(count(|| float_add(any(), any_float_const()).into_pattern(), &fx), 1);
    assert_eq!(count(|| float_binary(FloatBinaryOp::Add, any(), any()).into_pattern(), &fx), 1);
    assert_eq!(count(|| float_binary_any(any(), any()).into_pattern(), &fx), 1);

    // float_mul / float_div
    let fx = strider_ir_test_utils::make_fn_with_var(v, |b, base| {
        let f = fbase(b, base)?;
        let c = b.build_float_const(0x4000_0000_0000_0000, T::F64);
        let r = b.build_float_binary_op(f, c, FloatBinaryOp::Mul, T::F64)?;
        b.build_float_to_int(r, T::I64)
    })
    .unwrap()
    .0;
    assert_eq!(count(|| float_mul(any(), any()).into_pattern(), &fx), 1);

    let fx = strider_ir_test_utils::make_fn_with_var(v, |b, base| {
        let f = fbase(b, base)?;
        let c = b.build_float_const(0x4000_0000_0000_0000, T::F64);
        let r = b.build_float_binary_op(f, c, FloatBinaryOp::Div, T::F64)?;
        b.build_float_to_int(r, T::I64)
    })
    .unwrap()
    .0;
    assert_eq!(count(|| float_div(any(), any()).into_pattern(), &fx), 1);

    // float_neg / float_abs
    let fx = strider_ir_test_utils::make_fn_with_var(v, |b, base| {
        let f = fbase(b, base)?;
        let r = b.build_float_unary_op(f, FloatUnaryOp::Neg, T::F64)?;
        b.build_float_to_int(r, T::I64)
    })
    .unwrap()
    .0;
    assert_eq!(count(|| float_neg(any()).into_pattern(), &fx), 1);

    let fx = strider_ir_test_utils::make_fn_with_var(v, |b, base| {
        let f = fbase(b, base)?;
        let r = b.build_float_unary_op(f, FloatUnaryOp::Abs, T::F64)?;
        b.build_float_to_int(r, T::I64)
    })
    .unwrap()
    .0;
    assert_eq!(count(|| float_abs(any()).into_pattern(), &fx), 1);
    assert_eq!(count(|| strider_pattern::float_unary_any(any()).into_pattern(), &fx), 1);

    // float_sqrt / float_ceil / float_floor / float_round (each a plain
    // FloatUnaryOp variant).
    macro_rules! funary {
        ($variant:ident, $pat:expr) => {{
            let fx = strider_ir_test_utils::make_fn_with_var(v, |b, base| {
                let f = fbase(b, base)?;
                let r = b.build_float_unary_op(f, FloatUnaryOp::$variant, T::F64)?;
                b.build_float_to_int(r, T::I64)
            })
            .unwrap()
            .0;
            assert_eq!(count(|| $pat, &fx), 1, stringify!($variant));
        }};
    }
    funary!(Sqrt, float_sqrt(any()).into_pattern());
    funary!(Ceil, float_ceil(any()).into_pattern());
    funary!(Floor, float_floor(any()).into_pattern());
    funary!(Round, float_round(any()).into_pattern());

    // float_sub -> float_add(a, float_neg(b))
    let fx = strider_ir_test_utils::make_fn_with_var(v, |b, base| {
        let f = fbase(b, base)?;
        let c = b.build_float_const(0x4000_0000_0000_0000, T::F64);
        let neg = b.build_float_unary_op(c, FloatUnaryOp::Neg, T::F64)?;
        let r = b.build_float_binary_op(f, neg, FloatBinaryOp::Add, T::F64)?;
        b.build_float_to_int(r, T::I64)
    })
    .unwrap()
    .0;
    assert_eq!(count(|| float_sub(any(), any_float_const()).into_pattern(), &fx), 1);
}

#[test]
fn float_cmp_family() {
    let v = reg_vn(0, 8);
    // float_eq
    let fx = strider_ir_test_utils::make_fn_with_var(v, |b, base| {
        let f = b.build_int_to_float(base, T::F64)?;
        let c = b.build_float_const(0x4000_0000_0000_0000, T::F64);
        let cmp = b.build_float_cmp_op(f, c, FloatCmpOp::Equal)?;
        b.extend_if_needed(cmp, T::I64, ExtendOp::ZeroExtend)
    })
    .unwrap()
    .0;
    assert_eq!(count(|| float_eq(any(), any()).into_pattern(), &fx), 1);
    assert_eq!(count(|| float_cmp(FloatCmpOp::Equal, any(), any()).into_pattern(), &fx), 1);
    assert_eq!(count(|| float_cmp_any(any(), any()).into_pattern(), &fx), 1);

    // float_lt
    let fx = strider_ir_test_utils::make_fn_with_var(v, |b, base| {
        let f = b.build_int_to_float(base, T::F64)?;
        let c = b.build_float_const(0x4000_0000_0000_0000, T::F64);
        let cmp = b.build_float_cmp_op(f, c, FloatCmpOp::Less)?;
        b.extend_if_needed(cmp, T::I64, ExtendOp::ZeroExtend)
    })
    .unwrap()
    .0;
    assert_eq!(count(|| float_lt(any(), any()).into_pattern(), &fx), 1);
}

#[test]
fn float_lowered_shapes() {
    let v = reg_vn(0, 8);
    // float_ne(a, b) -> xor(float_eq(a, b), 1):I1
    let fx = strider_ir_test_utils::make_fn_with_var(v, |b, base| {
        let f = b.build_int_to_float(base, T::F64)?;
        let c = b.build_float_const(0x4000_0000_0000_0000, T::F64);
        let eq = b.build_float_cmp_op(f, c, FloatCmpOp::Equal)?;
        let one = b.build_int_const(1u64, T::I1)?;
        let ne = b.build_int_binary_operation(eq, one, IntBinaryOp::Xor, T::I1)?;
        b.extend_if_needed(ne, T::I64, ExtendOp::ZeroExtend)
    })
    .unwrap()
    .0;
    assert_eq!(count(|| float_ne(any(), any()).into_pattern(), &fx), 1);

    // float_is_nan(x) -> xor(float_eq(x, x), 1):I1
    let fx = strider_ir_test_utils::make_fn_with_var(v, |b, base| {
        let f = b.build_int_to_float(base, T::F64)?;
        let eq = b.build_float_cmp_op(f, f, FloatCmpOp::Equal)?;
        let one = b.build_int_const(1u64, T::I1)?;
        let nan = b.build_int_binary_operation(eq, one, IntBinaryOp::Xor, T::I1)?;
        b.extend_if_needed(nan, T::I64, ExtendOp::ZeroExtend)
    })
    .unwrap()
    .0;
    assert_eq!(count(|| float_is_nan(any()).into_pattern(), &fx), 1);

    // float_le(a, b) -> or(float_lt(a, b), float_eq(a, b)):I1
    let fx = strider_ir_test_utils::make_fn_with_var(v, |b, base| {
        let f = b.build_int_to_float(base, T::F64)?;
        let c = b.build_float_const(0x4000_0000_0000_0000, T::F64);
        let lt = b.build_float_cmp_op(f, c, FloatCmpOp::Less)?;
        let eq = b.build_float_cmp_op(f, c, FloatCmpOp::Equal)?;
        let le = b.build_int_binary_operation(lt, eq, IntBinaryOp::Or, T::I1)?;
        b.extend_if_needed(le, T::I64, ExtendOp::ZeroExtend)
    })
    .unwrap()
    .0;
    assert_eq!(count(|| float_le(any(), any()).into_pattern(), &fx), 1);
}

// ── Boolean ops ───────────────────────────────────────────────────────

#[test]
fn bool_family() {
    let v = reg_vn(0, 8);
    fn bbase(b: &mut strider_ir::FunctionBuilder, base: strider_ir::node::ValueId) -> anyhow::Result<(strider_ir::node::ValueId, strider_ir::node::ValueId)> {
        // Two I1 values: (base == 0) and (base < 0).
        let z = b.build_int_const(0u64, T::I64)?;
        let p = b.build_int_cmp_operation(base, z, IntCmpOp::Equal, T::I64)?;
        let q = b.build_int_cmp_operation(base, z, IntCmpOp::Less, T::I64)?;
        Ok((p, q))
    }
    macro_rules! boolbin {
        ($op:ident, $pat:expr) => {{
            let fx = strider_ir_test_utils::make_fn_with_var(v, |b, base| {
                let (p, q) = bbase(b, base)?;
                let r = b.build_int_binary_operation(p, q, IntBinaryOp::$op, T::I1)?;
                b.extend_if_needed(r, T::I64, ExtendOp::ZeroExtend)
            })
            .unwrap()
            .0;
            assert_eq!(count(|| $pat, &fx), 1, stringify!($op));
        }};
    }
    boolbin!(And, bool_and(bool_value(), bool_value()).into_pattern());
    boolbin!(Or, bool_or(bool_value(), bool_value()).into_pattern());
    boolbin!(Xor, bool_xor(bool_value(), bool_value()).into_pattern());
    boolbin!(And, bool_binary(IntBinaryOp::And, bool_value(), bool_value()).into_pattern());
    boolbin!(And, strider_pattern::bool_bin_any(bool_value(), bool_value()).into_pattern());

    // bool_not(x) -> xor(x, 1):I1
    let fx = strider_ir_test_utils::make_fn_with_var(v, |b, base| {
        let (p, _q) = bbase(b, base)?;
        let one = b.build_int_const(1u64, T::I1)?;
        let r = b.build_int_binary_operation(p, one, IntBinaryOp::Xor, T::I1)?;
        b.extend_if_needed(r, T::I64, ExtendOp::ZeroExtend)
    })
    .unwrap()
    .0;
    assert_eq!(count(|| bool_not(bool_value()).into_pattern(), &fx), 1);
}

#[test]
fn bool_binary_pins_i1_not_wide() {
    // A wide (I64) And must NOT match bool_and / bool_value-pinned ops.
    let v = reg_vn(0, 8);
    let fx = strider_ir_test_utils::make_fn_with_var(v, |b, base| {
        let k = b.build_int_const(3u64, T::I64)?;
        b.build_int_binary_operation(base, k, IntBinaryOp::And, T::I64)
    })
    .unwrap()
    .0;
    assert_eq!(count(|| bool_and(any(), any()).into_pattern(), &fx), 0);
    // But the plain int `and` does match it.
    assert_eq!(count(|| and(any(), any()).into_pattern(), &fx), 1);
}

// ── Consts ────────────────────────────────────────────────────────────

#[test]
fn const_family() {
    // signed_int_const recognises the zero-extended narrow form.
    let fx = make_empty_fn(|b| b.build_int_const(0xFFFF_FFCEu64, T::I64)).unwrap();
    assert_eq!(count(|| signed_int_const(-50).into_pattern(), &fx), 1);
    assert_eq!(count(|| any_int_const().into_pattern(), &fx), 1);
    assert_eq!(count(|| int_const_any_of([0xFFFF_FFCEu64]).into_pattern(), &fx), 1);

    // int_const_any_of preserves its value-set predicate at match time:
    // a constant outside the set must NOT match (regression: the build
    // refactor dropped the VariantWith predicate, so 7 matched [1,2,3]).
    let fx7 = make_empty_fn(|b| b.build_int_const(7u64, T::I64)).unwrap();
    assert_eq!(count(|| int_const_any_of([1u64, 2, 3]).into_pattern(), &fx7), 0);
    assert_eq!(count(|| int_const_any_of([1u64, 7, 3]).into_pattern(), &fx7), 1);

    // `int_const(u128::MAX)` matches a width-relative all-ones constant
    // (its match is width-masked, so all-ones at any width compares equal).
    let fx = make_empty_fn(|b| b.build_int_const(u128::MAX, T::I64)).unwrap();
    assert_eq!(count(|| int_const(u128::MAX).into_pattern(), &fx), 1);
    // A non-all-ones constant must not match.
    let fx = make_empty_fn(|b| b.build_int_const(5u64, T::I64)).unwrap();
    assert_eq!(count(|| int_const(u128::MAX).into_pattern(), &fx), 0);

    // bool_const / any_bool_const at I1.
    let fx = make_empty_fn(|b| b.build_int_const(1u64, T::I1)).unwrap();
    assert_eq!(count(|| bool_const(true).into_pattern(), &fx), 1);
    assert_eq!(count(|| any_bool_const().into_pattern(), &fx), 1);

    // float_const / any_float_const.
    let fx = make_empty_fn(|b| {
        let f = b.build_float_const(0x4000_0000_0000_0000, T::F64);
        b.build_float_to_int(f, T::I64)
    })
    .unwrap();
    assert_eq!(count(|| float_const(0x4000_0000_0000_0000).into_pattern(), &fx), 1);
    assert_eq!(count(|| any_float_const().into_pattern(), &fx), 1);
}

// ── Wildcards / width filters ─────────────────────────────────────────

#[test]
fn wildcard_family() {
    let v = reg_vn(0, 8);
    // value_of_width(1) / bool_value() select the I1 cmp output.
    let fx = strider_ir_test_utils::make_fn_with_var(v, |b, base| {
        let z = b.build_int_const(0u64, T::I64)?;
        let cmp = b.build_int_cmp_operation(base, z, IntCmpOp::Equal, T::I64)?;
        b.extend_if_needed(cmp, T::I64, ExtendOp::ZeroExtend)
    })
    .unwrap()
    .0;
    assert_eq!(count(|| bool_value().into_pattern(), &fx), 1);
    assert_eq!(count(|| value_of_width(1).into_pattern(), &fx), 1);

    // predicate gating on the matched output type. The I1-fallback the
    // matcher passes for non-value-output roots (Region / Return) means a
    // width-1 predicate also fires there, so gate on a 64-bit width to
    // pin a value-only property: InitialVar, IntConst, Phi, Extend are the
    // four I64-output nodes.
    assert_eq!(
        count(
            || predicate(|_m, ty| ty.bit_width() == 64).into_pattern(),
            &fx
        ),
        4
    );

    // inputs_of_width(1, …) / bool_inputs select an op operating on I1.
    let fx = strider_ir_test_utils::make_fn_with_var(v, |b, base| {
        let z = b.build_int_const(0u64, T::I64)?;
        let p = b.build_int_cmp_operation(base, z, IntCmpOp::Equal, T::I64)?;
        let q = b.build_int_cmp_operation(base, z, IntCmpOp::Less, T::I64)?;
        let r = b.build_int_binary_operation(p, q, IntBinaryOp::And, T::I1)?;
        b.extend_if_needed(r, T::I64, ExtendOp::ZeroExtend)
    })
    .unwrap()
    .0;
    // Two nodes have all-I1 value inputs: the `And` and the zero-extend
    // that widens it. Compose with a concrete inner to pin the `And`.
    assert_eq!(count(|| bool_inputs(any()).into_pattern(), &fx), 2);
    assert_eq!(count(|| inputs_of_width(1, any()).into_pattern(), &fx), 2);
    assert_eq!(
        count(|| bool_inputs(bool_and(any(), any())).into_pattern(), &fx),
        1
    );
}

#[test]
fn initial_var_family() {
    let rax = reg_vn(0, 8);
    let rbx = reg_vn(16, 8);
    let (fx, _val) = strider_ir_test_utils::make_fn_with_var(rax, |_b, base| Ok(base)).unwrap();
    assert_eq!(count(|| initial_var().into_pattern(), &fx), 1);
    assert_eq!(count(|| initial_var_for(rax).into_pattern(), &fx), 1);
    assert_eq!(count(|| initial_var_for(rbx).into_pattern(), &fx), 0);
}

// ── combinators ───────────────────────────────────────────────────────

#[test]
fn combinators_filter_and_guard() {
    let fx = make_empty_fn(|b| {
        let x = b.build_int_const(5u64, T::I64)?;
        let k = b.build_int_const(1u64, T::I64)?;
        b.build_int_binary_operation(x, k, IntBinaryOp::Add, T::I64)
    })
    .unwrap();
    // .filter rejecting everything -> 0 matches.
    assert_eq!(count(|| any().filter(|_m, _n| false).into_pattern(), &fx), 0);
    // .ordered on a commutative add still matches the natural order.
    let c = Capture::new();
    assert_eq!(
        count(|| add(int_const(5u128), int_const(1u128)).ordered().into_pattern(), &fx),
        1
    );
    // .capture on the root binds.
    let pat = add(var(c), any()).into_pattern();
    let hits = Matcher::try_new(&fx).unwrap().find_all(&pat).unwrap();
    assert_eq!(hits.len(), 1);
}

#[test]
fn of_width_root_and_nested() {
    let v = reg_vn(0, 8);
    // I1 cmp output, zero-extended to I64. The I64 value outputs are
    // InitialVar, IntConst, Phi, and Extend (matching the existing
    // `wildcard_family` predicate count of 4); the lone I1 is the cmp.
    let fx = strider_ir_test_utils::make_fn_with_var(v, |b, base| {
        let z = b.build_int_const(0u64, T::I64)?;
        let cmp = b.build_int_cmp_operation(base, z, IntCmpOp::Equal, T::I64)?;
        b.extend_if_needed(cmp, T::I64, ExtendOp::ZeroExtend)
    })
    .unwrap()
    .0;

    // .of_width at the ROOT: one 1-bit value output (the cmp).
    assert_eq!(count(|| any().of_width(1).into_pattern(), &fx), 1);
    // Four 64-bit value outputs.
    assert_eq!(count(|| any().of_width(64).into_pattern(), &fx), 4);
    // Width that matches nothing.
    assert_eq!(count(|| any().of_width(32).into_pattern(), &fx), 0);

    // .bool_valued sugar == .of_width(1).
    assert_eq!(count(|| any().bool_valued().into_pattern(), &fx), 1);

    // value_of_width / bool_value (re-expressed over the combinator) agree.
    assert_eq!(count(|| value_of_width(1).into_pattern(), &fx), 1);
    assert_eq!(count(|| bool_value().into_pattern(), &fx), 1);

    // .of_width NESTED inside an op: the zero-extend's operand must be I1.
    // Matches the Extend node exactly once.
    assert_eq!(count(|| zero_extend(any().of_width(1)).into_pattern(), &fx), 1);
    // Same op constrained to a non-matching operand width -> 0.
    assert_eq!(count(|| zero_extend(any().of_width(64)).into_pattern(), &fx), 0);
}

#[test]
fn output_ty_exact() {
    let v = reg_vn(0, 8);
    let fx = strider_ir_test_utils::make_fn_with_var(v, |b, base| {
        let z = b.build_int_const(0u64, T::I64)?;
        let cmp = b.build_int_cmp_operation(base, z, IntCmpOp::Equal, T::I64)?;
        b.extend_if_needed(cmp, T::I64, ExtendOp::ZeroExtend)
    })
    .unwrap()
    .0;
    // Exact-type match: one I1, four I64, zero I32.
    assert_eq!(count(|| any().value_ty(T::I1).into_pattern(), &fx), 1);
    assert_eq!(count(|| any().value_ty(T::I64).into_pattern(), &fx), 4);
    assert_eq!(count(|| any().value_ty(T::I32).into_pattern(), &fx), 0);
    // Nested under an op.
    assert_eq!(
        count(|| zero_extend(any().value_ty(T::I1)).into_pattern(), &fx),
        1
    );
}

#[test]
fn of_width_with_capture() {
    let v = reg_vn(0, 8);
    let fx = strider_ir_test_utils::make_fn_with_var(v, |b, base| {
        let z = b.build_int_const(0u64, T::I64)?;
        let cmp = b.build_int_cmp_operation(base, z, IntCmpOp::Equal, T::I64)?;
        b.extend_if_needed(cmp, T::I64, ExtendOp::ZeroExtend)
    })
    .unwrap()
    .0;
    // Constrain AND bind: the zero-extend's I1 operand bound to `c`.
    let c = Capture::new();
    let pat = zero_extend(var(c).of_width(1)).into_pattern();
    let hits = Matcher::try_new(&fx).unwrap().find_all(&pat).unwrap();
    assert_eq!(hits.len(), 1);
    // The bound node is the I1-producing comparison.
    let bound = hits[0].value(c).unwrap();
    assert_eq!(fx.value_kind(bound).as_value().unwrap(), T::I1);

    // A var(c).of_width(1) nested in an op behaves like the old
    // .when_match width check: a mismatched width fails the whole match.
    let pat_bad = zero_extend(var(c).of_width(64)).into_pattern();
    assert_eq!(Matcher::try_new(&fx).unwrap().find_all(&pat_bad).unwrap().len(), 0);
}

// ── test helpers ──────────────────────────────────────────────────────

/// Count reachable nodes by matching `any()`.
fn node_count(f: &strider_ir::Function) -> usize {
    let pat = any().into_pattern();
    Matcher::try_new(f).unwrap().find_all(&pat).unwrap().len()
}
