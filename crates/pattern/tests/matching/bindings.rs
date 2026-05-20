//! Direct tests for the `pattern::Bindings` public API.
//!
//! Covers the unified [`Capture`]-based binding contract:
//!
//!   * first bind returns `true` and is retrievable;
//!   * idempotent rebind returns `true`;
//!   * conflicting rebind returns `false` AND preserves the original;
//!   * typed extractors (`get_uint`, `get_bool`, `get_float_bits`,
//!     `get_int_binary_op`, …) read the bound node's `NodeKind`
//!     directly via the graph; unbound captures and shape-mismatched
//!     bindings both yield `None`.

use strider_ir::node::NodeKind;
use pattern::*;

use super::support::Tb;

// ── Capture (unified node + output) ──────────────────────────────────────────

#[test]
fn capture_bind_and_get_with_real_output_ids() {
    // Build `return(IntConst(1) + IntConst(2))` to harvest two distinct
    // `NodeOutputId`s from the graph.
    let mut t = Tb::empty();
    let a = t.u64(1);
    let b = t.u64(2);
    let s = t.add(a, b);
    let g = t.ret_val(s);

    let na = g.graph.get_node_from_output(a);
    let nb = g.graph.get_node_from_output(b);

    let mut bindings = Bindings::default();
    let v = Capture::new();
    assert_eq!(bindings.get(v), None);
    let ba = pattern::Binding::new(na, Some(a));
    let bb = pattern::Binding::new(nb, Some(b));
    assert!(bindings.bind_capture_for_test(v, ba));
    assert_eq!(bindings.get(v), Some(a));

    // Idempotent with same output.
    assert!(bindings.bind_capture_for_test(v, ba));
    assert_eq!(bindings.get(v), Some(a));

    // Conflict preserves original.
    assert!(!bindings.bind_capture_for_test(v, bb));
    assert_eq!(bindings.get(v), Some(a));
}

#[test]
fn capture_bind_and_get_with_real_node_ids() {
    // Thread distinct values through an Add so both constants stay reachable.
    let mut t = Tb::empty();
    let a = t.u64(1);
    let b = t.u64(2);
    let s = t.add(a, b);
    let g = t.ret_val(s);

    let mut ids = g
        .preorder()
        .filter(|&n| matches!(g.graph.node_kind(n), NodeKind::IntConst(_)));
    let n1 = ids.next().expect("first const node");
    let n2 = ids.next().expect("second const node");
    assert_ne!(n1, n2);

    let mut bindings = Bindings::default();
    let v = Capture::new();
    assert_eq!(bindings.get_node(v), None);
    let b1 = pattern::Binding::new(n1, None);
    let b2 = pattern::Binding::new(n2, None);
    assert!(bindings.bind_capture_for_test(v, b1));
    assert_eq!(bindings.get_node(v), Some(n1));
    assert!(bindings.bind_capture_for_test(v, b1));
    assert!(!bindings.bind_capture_for_test(v, b2));
    assert_eq!(bindings.get_node(v), Some(n1));
}

// ── Typed extractors (`get_uint` / `get_bool` / `get_float_bits` /
//    `get_*_op`) read through the graph ────────────────────────────────────────

#[test]
fn get_uint_reads_int_const_through_bound_capture() {
    let mut t = Tb::empty();
    let c = t.u64(7);
    let g = t.ret_val(c);
    let n = g.graph.get_node_from_output(c);

    let mut bindings = Bindings::default();
    let v = Capture::new();
    assert!(bindings.bind_capture_for_test(v, Binding::new(n, Some(c))));
    assert_eq!(bindings.get_uint(v, &g), Some(7));
}

#[test]
fn get_uint_returns_none_when_not_an_int_const() {
    let mut t = Tb::empty();
    let a = t.u64(1);
    let b = t.u64(2);
    let s = t.add(a, b);
    let g = t.ret_val(s);
    let add_node = g.graph.get_node_from_output(s);

    let mut bindings = Bindings::default();
    let v = Capture::new();
    assert!(bindings.bind_capture_for_test(v, Binding::new(add_node, Some(s))));
    assert_eq!(bindings.get_uint(v, &g), None);
}

#[test]
fn get_int_binary_op_reads_op_variant_through_bound_capture() {
    let mut t = Tb::empty();
    let a = t.u64(1);
    let b = t.u64(2);
    let s = t.add(a, b);
    let g = t.ret_val(s);
    let add_node = g.graph.get_node_from_output(s);

    let mut bindings = Bindings::default();
    let v = Capture::new();
    assert!(bindings.bind_capture_for_test(v, Binding::new(add_node, None)));
    assert_eq!(bindings.get_int_binary_op(v, &g), Some(IntBinaryOp::Add));
}

#[test]
fn unbound_capture_yields_none_for_every_typed_extractor() {
    let g = Tb::empty().ret_const(0);
    let bindings = Bindings::default();
    let v = Capture::new();
    assert_eq!(bindings.get(v), None);
    assert_eq!(bindings.get_node(v), None);
    assert_eq!(bindings.get_uint(v, &g), None);
    assert_eq!(bindings.get_int(v, &g), None);
    assert_eq!(bindings.get_bool(v, &g), None);
    assert_eq!(bindings.get_float_bits(v, &g), None);
    assert_eq!(bindings.get_int_binary_op(v, &g), None);
    assert_eq!(bindings.get_int_unary_op(v, &g), None);
    assert_eq!(bindings.get_int_cmp_op(v, &g), None);
    assert_eq!(bindings.get_bool_binary_op(v, &g), None);
    assert_eq!(bindings.get_bool_unary_op(v, &g), None);
    assert_eq!(bindings.get_float_binary_op(v, &g), None);
    assert_eq!(bindings.get_float_unary_op(v, &g), None);
    assert_eq!(bindings.get_float_cmp_op(v, &g), None);
}

// ── Globally unique IDs ──────────────────────────────────────────────────────

/// `Capture::new()` uses a process-wide atomic counter; allocating many
/// must produce all-distinct IDs.  `Debug` output is the only public
/// handle on the raw ID, so the test uses it as a set key.
#[test]
fn capture_ids_are_globally_unique_across_many_allocations() {
    const N: usize = 256;
    let mut ids: Vec<String> = Vec::with_capacity(N);
    for _ in 0..N {
        ids.push(format!("{:?}", Capture::new()));
    }
    let unique: std::collections::HashSet<&String> = ids.iter().collect();
    assert_eq!(unique.len(), ids.len());
}
