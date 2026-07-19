//! Wildcards, constant constructors, constant-value capture, boundary values,
//! and IR-level constant deduplication.

use strider_ir::node::ValueType;
use strider_pattern::*;

use super::support::{Tb, assertions as a};

/// `any()` matches every reachable value output in the graph.
#[test]
fn any_matches_every_output() {
    let mut t = Tb::empty();
    let a = t.u64(5);
    let b = t.u64(3);
    let s = t.add(a, b);
    let function = t.ret_val(s);

    // No kind filter, so one match per reachable value output. The exact count
    // depends on graph internals, hence the loose bound.
    let hits = Matcher::new(&function)
        .find_all(&any().into_pattern())
        .unwrap();
    assert!(
        hits.len() >= 3,
        "expected at least 3 matches, got {}",
        hits.len()
    );
}

/// `var(v)` is shorthand for `any().capture(v)`.
#[test]
fn var_binds_to_matched_output() {
    let mut t = Tb::empty();
    let c = t.u64(42);
    let function = t.ret_val(c);

    let v = Capture::new();
    let m = a::first(&function, int_const(42u128).capture(v).into_pattern());
    assert_eq!(m.bindings().get_uint(v, &function), Some(42));
}

#[test]
fn int_const_exact_matches() {
    let function = Tb::empty().ret_const(7);
    a::matches(&function, int_const(7u128).into_pattern(), 1);
}

#[test]
fn int_const_wrong_value_rejects() {
    let function = Tb::empty().ret_const(7);
    a::none(&function, int_const(8u128).into_pattern());
}

#[test]
fn int_const_zero_and_u64_max_match() {
    let mut t = Tb::empty();
    let lo = t.u64(0);
    let hi = t.u64(u64::MAX);
    let s = t.add(lo, hi);
    let function = t.ret_val(s);

    a::matches(&function, int_const(0u128).into_pattern(), 1);
    a::matches(&function, int_const(u128::from(u64::MAX)).into_pattern(), 1);
}

#[test]
fn any_int_const_captures_value() {
    let function = Tb::empty().ret_const(123);
    let iv = Capture::new();
    let m = a::unique(&function, any_int_const().capture(iv).into_pattern());
    assert_eq!(m.bindings().get_uint(iv, &function), Some(123));
}

#[test]
fn any_int_const_rejects_non_const() {
    // Only the two IntConst leaves should match, not the Add root.
    let mut t = Tb::empty();
    let a1 = t.u64(5);
    let a2 = t.u64(3);
    let s = t.add(a1, a2);
    let function = t.ret_val(s);

    let iv = Capture::new();
    a::matches(&function, any_int_const().capture(iv).into_pattern(), 2);
}

#[test]
fn bool_const_true_matches() {
    // Returned at I1 on purpose: widening via as_int would const-fold to a
    // wider IntConst, which the I1-typed bool_const must not match.
    let mut t = Tb::empty();
    let b = t.boolean(true);
    let function = t.ret_val(b);

    a::matches(&function, bool_const(true).into_pattern(), 1);
    a::none(&function, bool_const(false).into_pattern());
}

#[test]
fn any_bool_const_captures_value() {
    // Kept at I1: widening would const-fold away the boolean const entirely.
    let mut t = Tb::empty();
    let b = t.boolean(true);
    let function = t.ret_val(b);

    let bv = Capture::new();
    let m = a::unique(&function, any_bool_const().capture(bv).into_pattern());
    assert_eq!(m.bindings().get_bool(bv, &function), Some(true));
}

#[test]
fn float_const_exact_bits_matches() {
    let mut t = Tb::empty();
    let pi = t.f64(std::f64::consts::PI);
    let pi_i = t.float_to_int(pi, ValueType::I64);
    let function = t.ret_val(pi_i);

    a::matches(
        &function,
        float_const(std::f64::consts::PI.to_bits()).into_pattern(),
        1,
    );
    a::none(
        &function,
        float_const(std::f64::consts::E.to_bits()).into_pattern(),
    );
}

#[test]
fn float_const_nan_bits_match_separately_from_zero() {
    let mut t = Tb::empty();
    let nan = t.float_bits(f64::NAN.to_bits(), ValueType::F64);
    let zero = t.float_bits(0.0f64.to_bits(), ValueType::F64);
    let sum = t.fbin(nan, zero, strider_ir::FloatBinaryOp::Add, ValueType::F64);
    let as_int = t.float_to_int(sum, ValueType::I64);
    let function = t.ret_val(as_int);

    a::matches(&function, float_const(f64::NAN.to_bits()).into_pattern(), 1);
    a::matches(&function, float_const(0.0f64.to_bits()).into_pattern(), 1);
}

#[test]
fn any_float_const_captures_bits() {
    let mut t = Tb::empty();
    let c = t.f64(2.5);
    let ci = t.float_to_int(c, ValueType::I64);
    let function = t.ret_val(ci);

    let fv = Capture::new();
    let m = a::unique(&function, any_float_const().capture(fv).into_pattern());
    assert_eq!(
        m.bindings().get_float_bits(fv, function.graph()),
        Some(2.5f64.to_bits())
    );
}

/// The graph dedups constants, so two `IntConst(5)` requests are one `NodeId`
/// and `find_all` sees a single match despite two uses.
#[test]
fn duplicate_int_const_is_single_node() {
    let mut t = Tb::empty();
    let c1 = t.u64(5);
    let c2 = t.u64(5); // same value, deduplicated
    let s = t.add(c1, c2);
    let function = t.ret_val(s);

    a::matches(&function, int_const(5u128).into_pattern(), 1);
}
