use super::*;
use crate::var::{BoolVar, FloatVar, IntVar, NodeVar, Var};

// ── IntVar ────────────────────────────────────────────────────────────────

#[test]
fn int_var_bind_and_get() {
    let mut b = Bindings::default();
    let iv = IntVar::new();

    // Unbound → None.
    assert_eq!(b.get_int(iv), None);

    // First bind succeeds.
    assert!(b.bind_int(iv, 42));
    assert_eq!(b.get_int(iv), Some(42));
}

#[test]
fn int_var_idempotent_rebind() {
    let mut b = Bindings::default();
    let iv = IntVar::new();
    assert!(b.bind_int(iv, 42));
    // Rebinding to the same value is OK.
    assert!(b.bind_int(iv, 42));
    assert_eq!(b.get_int(iv), Some(42));
}

#[test]
fn int_var_conflict_fails() {
    let mut b = Bindings::default();
    let iv = IntVar::new();
    assert!(b.bind_int(iv, 42));
    // Rebinding to a different value fails.
    assert!(!b.bind_int(iv, 43));
    // The original binding is preserved after a conflict.
    assert_eq!(b.get_int(iv), Some(42));
}

// ── BoolVar ───────────────────────────────────────────────────────────────

#[test]
fn bool_var_bind_and_get() {
    let mut b = Bindings::default();
    let bv = BoolVar::new();

    assert_eq!(b.get_bool(bv), None);

    assert!(b.bind_bool(bv, true));
    assert_eq!(b.get_bool(bv), Some(true));
}

#[test]
fn bool_var_idempotent_rebind() {
    let mut b = Bindings::default();
    let bv = BoolVar::new();
    assert!(b.bind_bool(bv, false));
    assert!(b.bind_bool(bv, false));
    assert_eq!(b.get_bool(bv), Some(false));
}

#[test]
fn bool_var_conflict_fails() {
    let mut b = Bindings::default();
    let bv = BoolVar::new();
    assert!(b.bind_bool(bv, true));
    assert!(!b.bind_bool(bv, false));
    assert_eq!(b.get_bool(bv), Some(true));
}

// ── FloatVar ──────────────────────────────────────────────────────────────

#[test]
fn float_var_bind_and_get() {
    let mut b = Bindings::default();
    let fv = FloatVar::new();

    assert_eq!(b.get_float_bits(fv), None);

    // Use the IEEE 754 bit pattern for 1.0f64.
    let bits = 1.0f64.to_bits();
    assert!(b.bind_float(fv, bits));
    assert_eq!(b.get_float_bits(fv), Some(bits));
}

#[test]
fn float_var_idempotent_rebind() {
    let mut b = Bindings::default();
    let fv = FloatVar::new();
    let bits = 2.0f64.to_bits();
    assert!(b.bind_float(fv, bits));
    assert!(b.bind_float(fv, bits));
    assert_eq!(b.get_float_bits(fv), Some(bits));
}

#[test]
fn float_var_conflict_fails() {
    let mut b = Bindings::default();
    let fv = FloatVar::new();
    let bits_a = 1.0f64.to_bits();
    let bits_b = 2.0f64.to_bits();
    assert!(b.bind_float(fv, bits_a));
    assert!(!b.bind_float(fv, bits_b));
    assert_eq!(b.get_float_bits(fv), Some(bits_a));
}

// ── IDs are globally unique across types ──────────────────────────────────

#[test]
fn capture_ids_are_globally_unique() {
    // Each call to ::new() increments the shared counter.  The only
    // guarantee we need is that two successive calls produce different ids.
    let iv = IntVar::new();
    let bv = BoolVar::new();
    let fv = FloatVar::new();
    let v = Var::new();
    let nv = NodeVar::new();
    // All five must be distinct (compared as their raw u32 inner values by
    // verifying each pair is not identical when cast to the same type as u32).
    // We expose no public field, so we use Debug output as a proxy.
    let ids: Vec<String> = vec![
        format!("{iv:?}"),
        format!("{bv:?}"),
        format!("{fv:?}"),
        format!("{v:?}"),
        format!("{nv:?}"),
    ];
    let unique: std::collections::HashSet<_> = ids.iter().collect();
    assert_eq!(unique.len(), ids.len(), "all capture IDs must be globally unique");
}
