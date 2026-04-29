//! Capture variables (`Var`, `NodeVar`, typed op-vars) and `.when` /
//! `predicate(f)` guards.
//!
//! Covers: identity enforcement across multiple occurrences, node-id capture,
//! root-level and sub-pattern predicates, composition of `.capture().when()`,
//! and graph-lookup helpers (`get_int_const`, `get_bool_const`,
//! `get_float_bits`) — including the "unbound var returns None" contract.

use ir::node::NodeOutputType;
use pattern::*;

use super::support::{Tb, assertions as a, shapes};

// ── Var equality enforcement ─────────────────────────────────────────────────

#[test]
fn same_var_twice_matches_identical_output() {
    // add(5, 5): both operands dedup to the same `NodeOutputId`.
    let g = shapes::add_consts(5, 5);
    let x = Var::new();
    a::matches(&g, add(var(x), var(x)), 1);
}

#[test]
fn same_var_twice_rejects_distinct_outputs() {
    // add(5, 3): operands are distinct.
    let g = shapes::add_consts(5, 3);
    let x = Var::new();
    a::none(&g, add(var(x), var(x)));
}

#[test]
fn var_used_three_times_enforces_all() {
    // add(add(7, 7), 7) — three uses of the same constant.
    let mut t = Tb::empty();
    let c = t.u64(7);
    let s = t.add(c, c);
    let s = t.add(s, c);
    let g = t.ret_val(s);

    let x = Var::new();
    let m = a::unique(&g, add(add(var(x), var(x)), var(x)));
    assert_eq!(m.get_int_const(x, &g), Some(7));
}

// ── NodeVar ──────────────────────────────────────────────────────────────────

#[test]
fn node_var_captures_node_id() {
    let g = shapes::call_at(0xABCD);
    let n = NodeVar::new();
    let m = a::unique(&g, call().at(0xABCD).capture_node(n));
    let node = m.get_node(n).expect("call node");
    assert!(matches!(g.graph.node_kind(node), ir::node::NodeKind::Call));
}

// ── Predicates: `.when` on root pattern ──────────────────────────────────────

#[test]
fn when_true_passes_match_through() {
    let g = shapes::add_consts(5, 3);
    a::matches(&g, add(int_const(5), int_const(3)).when(|_g, _ty, _o| true), 1);
}

#[test]
fn when_false_rejects_match() {
    let g = shapes::add_consts(5, 3);
    a::none(&g, add(int_const(5), int_const(3)).when(|_g, _ty, _o| false));
}

// ── `.when` on sub-pattern ───────────────────────────────────────────────────

#[test]
fn when_on_subpattern_filters() {
    let g = shapes::add_consts(5, 3);
    // Inner pattern requires the int_const(5) but rejects via when.
    a::none(
        &g,
        add(int_const(5).when(|_g, _ty, _o| false), int_const(3)),
    );
    // Same pattern with a pass-through when succeeds.
    a::matches(
        &g,
        add(int_const(5).when(|_g, _ty, _o| true), int_const(3)),
        1,
    );
}

// ── `predicate(f)` standalone ────────────────────────────────────────────────

#[test]
fn predicate_true_matches_all_outputs() {
    let g = shapes::add_consts(5, 3);
    let hits = Matcher::new(&g).find_all(&predicate(|_g, _ty, _o| true));
    assert!(!hits.is_empty());
}

#[test]
fn predicate_false_matches_nothing() {
    let g = shapes::add_consts(5, 3);
    a::matches(&g, predicate(|_g, _ty, _o| false), 0);
}

// ── Predicate reads the captured value ───────────────────────────────────────

#[test]
fn predicate_inspects_node_kind() {
    // Find any IntConst output with value == 7.
    let mut t = Tb::empty();
    let a_ = t.u64(7);
    let b_ = t.u64(3);
    let s = t.add(a_, b_);
    let g = t.ret_val(s);

    let hits = Matcher::new(&g).find_all(&predicate(|graph, _ty, o| {
        matches!(graph.graph.kind_of_output(o), ir::node::NodeKind::IntConst(7))
    }));
    assert_eq!(hits.len(), 1);
}

// ── `.capture(v).when(f)` composition ────────────────────────────────────────

#[test]
fn capture_then_when_composes() {
    let g = shapes::add_consts(5, 3);
    let x = Var::new();
    // Root matches; the predicate later inspects the capture and filters.
    let hits = Matcher::new(&g).find_all(
        &add(int_const(5), int_const(3))
            .capture(x)
            .when(|_g, _ty, _o| true),
    );
    assert_eq!(hits.len(), 1);
    assert!(hits[0].get(x).is_some());
}

// ── Match helper coverage ────────────────────────────────────────────────────

#[test]
fn get_int_const_returns_value() {
    let g = shapes::add_consts(5, 3);
    let x = Var::new();
    let m = a::unique(&g, add(var(x), int_const(3)));
    assert_eq!(m.get_int_const(x, &g), Some(5));
}

#[test]
fn get_int_const_on_non_const_returns_none() {
    // Capture the `Add` itself (not a constant), then ask get_int_const.
    let g = shapes::add_consts(5, 3);
    let x = Var::new();
    let m = a::unique(&g, add(int_const(5), int_const(3)).capture(x));
    assert_eq!(m.get_int_const(x, &g), None);
}

#[test]
fn get_int_const_on_unbound_var_returns_none() {
    let g = shapes::add_consts(5, 3);
    let m = a::first(&g, int_const(5));
    let never_bound = Var::new();
    assert_eq!(m.get_int_const(never_bound, &g), None);
    assert_eq!(m.get(never_bound), None);
}

#[test]
fn get_bool_const_and_float_bits_helpers() {
    let mut t = Tb::empty();
    let bc = t.boolean(true);
    let as_int = t.as_int(bc, NodeOutputType::U64);
    let g = t.ret_val(as_int);

    let v = Var::new();
    let m = a::unique(&g, bool_const(true).capture(v));
    assert_eq!(m.get_bool_const(v, &g), Some(true));
    // Not a float.
    assert_eq!(m.get_float_bits(v, &g), None);
}

#[test]
fn get_node_on_unbound_returns_none() {
    let g = shapes::call_at(0xABCD);
    let m = a::first(&g, call());
    let never_bound = NodeVar::new();
    assert_eq!(m.get_node(never_bound), None);
}

#[test]
fn match_root_is_the_matched_node() {
    let g = shapes::add_consts(5, 3);
    let m = a::unique(&g, add(int_const(5), int_const(3)));
    // The matched root should be an Add.
    assert!(matches!(
        g.graph.node_kind(m.root),
        ir::node::NodeKind::IntBinaryOp(ir::IntBinaryOp::Add)
    ));
}
