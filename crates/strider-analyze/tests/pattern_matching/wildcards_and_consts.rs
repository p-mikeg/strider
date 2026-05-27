//! Wildcards, constant constructors, constant-value capture.
//!
//! Covers: `any()`, `var(v)`, `int_const(n)`, `bool_const(b)`,
//! `float_const(bits)`, `any_int_const/any_bool_const/any_float_const`,
//! boundary values, and IR-level constant deduplication.

use strider_analyze::pattern::*;
use strider_ir::node::NodeOutputType;

use super::support::{Tb, assertions as a};

// ── `any()` and `var(v)` ─────────────────────────────────────────────────────

/// `any()` matches every reachable value output in the graph.
#[test]
fn any_matches_every_output() {
    // Graph: return(add(5, 3)) — 3 value outputs: two IntConsts and one Add.
    let mut t = Tb::empty();
    let a = t.u64(5);
    let b = t.u64(3);
    let s = t.add(a, b);
    let function = t.ret_val(s);

    // `any()` has no kind filter so it returns a match per reachable output.
    // The exact count depends on graph internals (Entry/Return produce control
    // edges, not value outputs) so we only require >= 3.
    let hits = Matcher::try_new(&function).unwrap().find_all(&any());
    assert!(hits.len() >= 3, "expected at least 3 matches, got {}", hits.len());
}

/// `var(v)` is shorthand for `any().capture(v)`.
#[test]
fn var_binds_to_matched_output() {
    let mut t = Tb::empty();
    let c = t.u64(42);
    let function = t.ret_val(c);

    let v = Capture::new();
    let m = a::first(&function, int_const(42u64).capture(v));
    assert_eq!(m.get_uint(v, &function), Some(42));
}

// ── Integer constants ─────────────────────────────────────────────────────────

#[test]
fn int_const_exact_matches() {
    let function = Tb::empty().ret_const(7);
    a::matches(&function, int_const(7u64), 1);
}

#[test]
fn int_const_wrong_value_rejects() {
    let function = Tb::empty().ret_const(7);
    a::none(&function, int_const(8u64));
}

#[test]
fn int_const_zero_and_u64_max_match() {
    let mut t = Tb::empty();
    let lo = t.u64(0);
    let hi = t.u64(u64::MAX);
    let s = t.add(lo, hi);
    let function = t.ret_val(s);

    a::matches(&function, int_const(0u64), 1);
    a::matches(&function, int_const(u64::MAX), 1);
}

#[test]
fn any_int_const_captures_value() {
    let function = Tb::empty().ret_const(123);
    let iv = Capture::new();
    let m = a::unique(&function, any_int_const(iv));
    assert_eq!(m.get_uint(iv, &function), Some(123));
}

#[test]
fn any_int_const_rejects_non_const() {
    // add(5, 3) has an Add root; any_int_const should only match the
    // IntConst leaves — two matches total.
    let mut t = Tb::empty();
    let a1 = t.u64(5);
    let a2 = t.u64(3);
    let s = t.add(a1, a2);
    let function = t.ret_val(s);

    let iv = Capture::new();
    a::matches(&function, any_int_const(iv), 2);
}

// ── Boolean constants ─────────────────────────────────────────────────────────

#[test]
fn bool_const_true_matches() {
    let mut t = Tb::empty();
    let b = t.boolean(true);
    let as_int = t.as_int(b, NodeOutputType::I64);
    let function = t.ret_val(as_int);

    a::matches(&function, bool_const(true), 1);
    a::none(&function, bool_const(false));
}

#[test]
fn any_bool_const_captures_value() {
    // Return the I1 boolean const directly: widening it to a wider integer
    // (via `as_int`) would const-fold it into a wider `IntConst`, leaving no
    // reachable I1 boolean const for `any_bool_const` to match.
    let mut t = Tb::empty();
    let b = t.boolean(true);
    let function = t.ret_val(b);

    let bv = Capture::new();
    let m = a::unique(&function, any_bool_const(bv));
    assert_eq!(m.get_bool(bv, &function), Some(true));
}

// ── Float constants ───────────────────────────────────────────────────────────

#[test]
fn float_const_exact_bits_matches() {
    let mut t = Tb::empty();
    let pi = t.f64(std::f64::consts::PI);
    let pi_i = t.float_to_int(pi, NodeOutputType::I64);
    let function = t.ret_val(pi_i);

    a::matches(&function, float_const(std::f64::consts::PI.to_bits()), 1);
    a::none(&function, float_const(std::f64::consts::E.to_bits()));
}

#[test]
fn float_const_nan_bits_match_separately_from_zero() {
    let mut t = Tb::empty();
    let nan = t.float_bits(f64::NAN.to_bits(), NodeOutputType::F64);
    let zero = t.float_bits(0.0f64.to_bits(), NodeOutputType::F64);
    let sum = t.fbin(nan, zero, strider_ir::FloatBinaryOp::Add, NodeOutputType::F64);
    let as_int = t.float_to_int(sum, NodeOutputType::I64);
    let function = t.ret_val(as_int);

    a::matches(&function, float_const(f64::NAN.to_bits()), 1);
    a::matches(&function, float_const(0.0f64.to_bits()), 1);
}

#[test]
fn any_float_const_captures_bits() {
    let mut t = Tb::empty();
    let c = t.f64(2.5);
    let ci = t.float_to_int(c, NodeOutputType::I64);
    let function = t.ret_val(ci);

    let fv = Capture::new();
    let m = a::unique(&function, any_float_const(fv));
    assert_eq!(m.get_float_bits(fv, &function), Some(2.5f64.to_bits()));
}

// ── Constant deduplication ────────────────────────────────────────────────────

/// The IR graph deduplicates constants: two `IntConst(5)` requests produce
/// the same `NodeId`.  `find_all(int_const(5))` must therefore return exactly
/// one match even when the value is used twice.
#[test]
fn duplicate_int_const_is_single_node() {
    let mut t = Tb::empty();
    let c1 = t.u64(5);
    let c2 = t.u64(5); // same value — deduplicated
    let s = t.add(c1, c2);
    let function = t.ret_val(s);

    a::matches(&function, int_const(5u64), 1);
}
