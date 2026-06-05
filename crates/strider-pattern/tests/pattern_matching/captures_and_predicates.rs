//! Capture variables (`Capture`, typed op-vars) and `.when` /
//! `predicate(f)` guards.
//!
//! Covers: identity enforcement across multiple occurrences, node-id capture,
//! root-level and sub-pattern predicates, composition of `.capture().when()`,
//! and graph-lookup helpers (`get_uint`, `get_bool`, `get_float_bits`) —
//! including the "unbound var returns None" contract.

use strider_pattern::*;
use strider_ir::IRViewer;
use strider_ir::node::IntPayload;

use super::support::{Tb, assertions as a, shapes};

// ── Capture equality enforcement ─────────────────────────────────────────────────

#[test]
fn same_var_twice_matches_identical_output() {
    // add(5, 5): both operands dedup to the same `ValueId`.
    let function = shapes::add_consts(5, 5);
    let x = Capture::new();
    a::matches(&function, add(var(x), var(x)).into_pattern(), 1);
}

#[test]
fn same_var_twice_rejects_distinct_outputs() {
    // add(5, 3): operands are distinct.
    let function = shapes::add_consts(5, 3);
    let x = Capture::new();
    a::none(&function, add(var(x), var(x)).into_pattern());
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
    let m = a::unique(&function, add(add(var(x), var(x)), var(x)).into_pattern());
    assert_eq!(m.bindings().get_uint(x, &function), Some(7));
}

// ── Capture binding for node-only patterns ───────────────────────────────────

#[test]
fn node_var_captures_node_id() {
    let function = shapes::call_at(0xABCD);
    let n = Capture::new();
    let m = a::unique(&function, call().at(0xABCD).capture(n).build());
    let node = m.node(n, function.graph()).expect("call node");
    assert!(matches!(function.node_kind(node), strider_ir::node::NodeKind::Call));
}

// ── Predicates: `.when` on root pattern ──────────────────────────────────────

#[test]
fn when_true_passes_match_through() {
    let function = shapes::add_consts(5, 3);
    a::matches(
        &function,
        add(int_const(5u128), int_const(3u128)).when_match(|_m, _ty, _b| true).into_pattern(),
        1,
    );
}

#[test]
fn when_false_rejects_match() {
    let function = shapes::add_consts(5, 3);
    a::none(
        &function,
        add(int_const(5u128), int_const(3u128)).when_match(|_m, _ty, _b| false).into_pattern(),
    );
}

// ── `.when` on sub-pattern ───────────────────────────────────────────────────

#[test]
fn when_on_subpattern_filters() {
    let function = shapes::add_consts(5, 3);
    // Inner pattern requires the int_const(5) but rejects via when.
    a::none(
        &function,
        add(int_const(5u128).when_match(|_m, _ty, _b| false), int_const(3u128)).into_pattern(),
    );
    // Same pattern with a pass-through when succeeds.
    a::matches(
        &function,
        add(int_const(5u128).when_match(|_m, _ty, _b| true), int_const(3u128)).into_pattern(),
        1,
    );
}

// ── `predicate(f)` standalone ────────────────────────────────────────────────

#[test]
fn predicate_true_matches_all_outputs() {
    let function = shapes::add_consts(5, 3);
    let hits = Matcher::try_new(&function)
        .unwrap()
        .find_all(&predicate(|_m, _ty| true).into_pattern()).unwrap();
    assert!(!hits.is_empty());
}

#[test]
fn predicate_false_matches_nothing() {
    let function = shapes::add_consts(5, 3);
    a::matches(&function, predicate(|_m, _ty| false).into_pattern(), 0);
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

    // The new predicate signature only sees `(matcher, ty)` — to filter on the
    // matched node's output id we capture it and check via the bindings in
    // `when_match`.
    let c = Capture::new();
    let hits = Matcher::try_new(&function).unwrap().find_all(
        &any()
            .capture(c)
            .when_match(move |m, _ty, b| {
                let Some(o) = b.get_value(c) else {
                    return false;
                };
                matches!(m.function().kind_of_value(o), strider_ir::node::NodeKind::IntConst(IntPayload::Small(7)))
            })
            .into_pattern(),
    ).unwrap();
    assert_eq!(hits.len(), 1);
}

// ── `.capture(v).when(f)` composition ────────────────────────────────────────

#[test]
fn capture_then_when_composes() {
    let function = shapes::add_consts(5, 3);
    let x = Capture::new();
    // Root matches; the predicate later inspects the capture and filters.
    let hits = Matcher::try_new(&function).unwrap().find_all(
        &add(int_const(5u128), int_const(3u128))
            .capture(x)
            .when_match(|_m, _ty, _b| true)
            .into_pattern(),
    ).unwrap();
    assert_eq!(hits.len(), 1);
    assert!(hits[0].value(x).is_some());
}

// ── Match helper coverage ────────────────────────────────────────────────────

#[test]
fn get_int_const_returns_value() {
    let function = shapes::add_consts(5, 3);
    let x = Capture::new();
    let m = a::unique(&function, add(var(x), int_const(3u128)).into_pattern());
    assert_eq!(m.bindings().get_uint(x, &function), Some(5));
}

#[test]
fn get_int_const_on_non_const_returns_none() {
    // Capture the `Add` itself (not a constant), then ask get_uint.
    let function = shapes::add_consts(5, 3);
    let x = Capture::new();
    let m = a::unique(&function, add(int_const(5u128), int_const(3u128)).capture(x).into_pattern());
    assert_eq!(m.bindings().get_uint(x, &function), None);
}

#[test]
fn get_int_const_on_unbound_var_returns_none() {
    let function = shapes::add_consts(5, 3);
    let m = a::first(&function, int_const(5u128).into_pattern());
    let never_bound = Capture::new();
    assert_eq!(m.bindings().get_uint(never_bound, &function), None);
    assert_eq!(m.value(never_bound), None);
}

#[test]
fn get_bool_const_and_float_bits_helpers() {
    // Return the I1 boolean const directly.  Widening it to a wider integer
    // would const-fold it into a wider `IntConst`, and `get_bool` only reads
    // back an `IntConst` typed `I1`.
    let mut t = Tb::empty();
    let bc = t.boolean(true);
    let function = t.ret_val(bc);

    let v = Capture::new();
    let m = a::unique(&function, bool_const(true).capture(v).into_pattern());
    assert_eq!(m.bindings().get_bool(v, &function), Some(true));
    // Not a float.
    assert_eq!(m.bindings().get_float_bits(v, function.graph()), None);
}

#[test]
fn get_node_on_unbound_returns_none() {
    let function = shapes::call_at(0xABCD);
    let m = a::first(&function, call().build());
    let never_bound = Capture::new();
    assert_eq!(m.node(never_bound, function.graph()), None);
}

#[test]
fn match_root_is_the_matched_node() {
    let function = shapes::add_consts(5, 3);
    let m = a::unique(&function, add(int_const(5u128), int_const(3u128)).into_pattern());
    // The matched root should be an Add.
    assert!(matches!(
        function.node_kind(m.root()),
        strider_ir::node::NodeKind::IntBinaryOp(strider_ir::IntBinaryOp::Add)
    ));
}

// ── filter() short-circuits before child recursion ───────────────────────────

/// A parent's `filter` runs after the kind / output-type check and BEFORE
/// the matcher recurses into the child sub-patterns: when the root filter
/// rejects, the child's own filter must NOT fire even once.
#[test]
fn filter_short_circuits_before_child_recursion() {
    use std::cell::Cell;
    use std::rc::Rc;

    let function = shapes::add_consts(5, 7);

    let child_invocations: Rc<Cell<usize>> = Rc::new(Cell::new(0));
    let counter = child_invocations.clone();
    let child = any().filter(move |_m, _n| {
        counter.set(counter.get() + 1);
        true
    });
    // Root's filter always fails, BEFORE walking the child.
    let root = add(int_const(99u128), child).filter(|_m, _n| false);

    a::none(&function, root.into_pattern());
    assert_eq!(
        child_invocations.get(),
        0,
        "child filter must NOT fire when the root filter short-circuits",
    );
}

/// Companion: when the root filter accepts, the match proceeds and the
/// child filter is visited.
#[test]
fn filter_accepts_match_and_visits_child() {
    use std::cell::Cell;
    use std::rc::Rc;

    let function = shapes::add_consts(5, 7);

    let child_invocations: Rc<Cell<usize>> = Rc::new(Cell::new(0));
    let counter = child_invocations.clone();
    let child = any().filter(move |_m, _n| {
        counter.set(counter.get() + 1);
        true
    });
    let root = add(int_const(5u128), child).filter(|_m, _n| true);

    a::matches(&function, root.into_pattern(), 1);
    assert!(
        child_invocations.get() >= 1,
        "child filter fires once child recursion proceeds (got {})",
        child_invocations.get(),
    );
}
