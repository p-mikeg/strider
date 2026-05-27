//! Capture variables (`Capture`, typed op-vars) and `.when` /
//! `predicate(f)` guards.
//!
//! Covers: identity enforcement across multiple occurrences, node-id capture,
//! root-level and sub-pattern predicates, composition of `.capture().when()`,
//! and graph-lookup helpers (`get_uint`, `get_bool`, `get_float_bits`) —
//! including the "unbound var returns None" contract.

use strider_analyze::pattern::*;
use strider_ir::node::NodeOutputType;

use super::support::{Tb, assertions as a, shapes};

// ── Capture equality enforcement ─────────────────────────────────────────────────

#[test]
fn same_var_twice_matches_identical_output() {
    // add(5, 5): both operands dedup to the same `NodeOutputId`.
    let function = shapes::add_consts(5, 5);
    let x = Capture::new();
    a::matches(&function, add(var(x), var(x)), 1);
}

#[test]
fn same_var_twice_rejects_distinct_outputs() {
    // add(5, 3): operands are distinct.
    let function = shapes::add_consts(5, 3);
    let x = Capture::new();
    a::none(&function, add(var(x), var(x)));
}

#[test]
fn var_used_three_times_enforces_all() {
    // add(add(7, 7), 7) — three uses of the same constant.
    let mut t = Tb::empty();
    let c = t.u64(7);
    let s = t.add(c, c);
    let s = t.add(s, c);
    let function = t.ret_val(s);

    let x = Capture::new();
    let m = a::unique(&function, add(add(var(x), var(x)), var(x)));
    assert_eq!(m.get_uint(x, &function), Some(7));
}

// ── Capture binding for node-only patterns ───────────────────────────────────

#[test]
fn node_var_captures_node_id() {
    let function = shapes::call_at(0xABCD);
    let n = Capture::new();
    let m = a::unique(&function, call().at(0xABCD).capture(n));
    let node = m.node(n).expect("call node");
    assert!(matches!(function.node_kind(node), strider_ir::node::NodeKind::Call));
}

// ── Predicates: `.when` on root pattern ──────────────────────────────────────

#[test]
fn when_true_passes_match_through() {
    let function = shapes::add_consts(5, 3);
    a::matches(&function, add(int_const(5), int_const(3)).when(|_g, _ty, _o| true), 1);
}

#[test]
fn when_false_rejects_match() {
    let function = shapes::add_consts(5, 3);
    a::none(&function, add(int_const(5), int_const(3)).when(|_g, _ty, _o| false));
}

// ── `.when` on sub-pattern ───────────────────────────────────────────────────

#[test]
fn when_on_subpattern_filters() {
    let function = shapes::add_consts(5, 3);
    // Inner pattern requires the int_const(5) but rejects via when.
    a::none(
        &function,
        add(int_const(5).when(|_g, _ty, _o| false), int_const(3)),
    );
    // Same pattern with a pass-through when succeeds.
    a::matches(
        &function,
        add(int_const(5).when(|_g, _ty, _o| true), int_const(3)),
        1,
    );
}

// ── `predicate(f)` standalone ────────────────────────────────────────────────

#[test]
fn predicate_true_matches_all_outputs() {
    let function = shapes::add_consts(5, 3);
    let hits = Matcher::try_new(&function).unwrap().find_all(&predicate(|_g, _ty, _o| true));
    assert!(!hits.is_empty());
}

#[test]
fn predicate_false_matches_nothing() {
    let function = shapes::add_consts(5, 3);
    a::matches(&function, predicate(|_g, _ty, _o| false), 0);
}

// ── Predicate reads the captured value ───────────────────────────────────────

#[test]
fn predicate_inspects_node_kind() {
    // Find any IntConst output with value == 7.
    let mut t = Tb::empty();
    let a_ = t.u64(7);
    let b_ = t.u64(3);
    let s = t.add(a_, b_);
    let function = t.ret_val(s);

    let hits = Matcher::try_new(&function).unwrap().find_all(&predicate(|graph, _ty, o| {
        matches!(graph.kind_of_output(o), strider_ir::node::NodeKind::IntConst(7))
    }));
    assert_eq!(hits.len(), 1);
}

// ── `.capture(v).when(f)` composition ────────────────────────────────────────

#[test]
fn capture_then_when_composes() {
    let function = shapes::add_consts(5, 3);
    let x = Capture::new();
    // Root matches; the predicate later inspects the capture and filters.
    let hits = Matcher::try_new(&function).unwrap().find_all(
        &add(int_const(5), int_const(3))
            .capture(x)
            .when(|_g, _ty, _o| true),
    );
    assert_eq!(hits.len(), 1);
    assert!(hits[0].output(x).is_some());
}

// ── Match helper coverage ────────────────────────────────────────────────────

#[test]
fn get_int_const_returns_value() {
    let function = shapes::add_consts(5, 3);
    let x = Capture::new();
    let m = a::unique(&function, add(var(x), int_const(3)));
    assert_eq!(m.get_uint(x, &function), Some(5));
}

#[test]
fn get_int_const_on_non_const_returns_none() {
    // Capture the `Add` itself (not a constant), then ask get_uint.
    let function = shapes::add_consts(5, 3);
    let x = Capture::new();
    let m = a::unique(&function, add(int_const(5), int_const(3)).capture(x));
    assert_eq!(m.get_uint(x, &function), None);
}

#[test]
fn get_int_const_on_unbound_var_returns_none() {
    let function = shapes::add_consts(5, 3);
    let m = a::first(&function, int_const(5));
    let never_bound = Capture::new();
    assert_eq!(m.get_uint(never_bound, &function), None);
    assert_eq!(m.output(never_bound), None);
}

#[test]
fn get_bool_const_and_float_bits_helpers() {
    let mut t = Tb::empty();
    let bc = t.boolean(true);
    let as_int = t.as_int(bc, NodeOutputType::I64);
    let function = t.ret_val(as_int);

    let v = Capture::new();
    let m = a::unique(&function, bool_const(true).capture(v));
    assert_eq!(m.get_bool(v, &function), Some(true));
    // Not a float.
    assert_eq!(m.get_float_bits(v, &function), None);
}

#[test]
fn get_node_on_unbound_returns_none() {
    let function = shapes::call_at(0xABCD);
    let m = a::first(&function, call());
    let never_bound = Capture::new();
    assert_eq!(m.node(never_bound), None);
}

#[test]
fn match_root_is_the_matched_node() {
    let function = shapes::add_consts(5, 3);
    let m = a::unique(&function, add(int_const(5), int_const(3)));
    // The matched root should be an Add.
    assert!(matches!(
        function.node_kind(m.root()),
        strider_ir::node::NodeKind::IntBinaryOp(strider_ir::IntBinaryOp::Add)
    ));
}
