use strider_ir::IRViewer;
use strider_pattern::*;

use super::support::{Tb, assertions as a, shapes};

#[test]
fn same_var_twice_matches_identical_output() {
    // Both operands of int_add(5, 5) dedup to the same `ValueId`.
    let function = shapes::add_consts(5, 5);
    let x = Capture::new();
    a::matches(&function, int_add(var(x), var(x)).into_pattern(), 1);
}

#[test]
fn same_var_twice_rejects_distinct_outputs() {
    let function = shapes::add_consts(5, 3);
    let x = Capture::new();
    a::none(&function, int_add(var(x), var(x)).into_pattern());
}

#[test]
fn var_used_three_times_enforces_all() {
    let mut t = Tb::empty();
    let c = t.u64(7);
    let s = t.add(c, c);
    let s = t.add(s, c);
    let function = t.ret_val(s);

    let x = Capture::new();
    assert_eq!(
        a::unique_uint(
            &function,
            int_add(int_add(var(x), var(x)), var(x)).into_pattern(),
            x
        ),
        Some(7),
    );
}

#[test]
fn when_true_passes_match_through() {
    let function = shapes::add_consts(5, 3);
    a::matches(
        &function,
        int_add(int_const(5u128), int_const(3u128))
            .when_match(|_m, _ty, _b| true)
            .into_pattern(),
        1,
    );
}

#[test]
fn when_false_rejects_match() {
    let function = shapes::add_consts(5, 3);
    a::none(
        &function,
        int_add(int_const(5u128), int_const(3u128))
            .when_match(|_m, _ty, _b| false)
            .into_pattern(),
    );
}

#[test]
fn when_on_subpattern_filters() {
    let function = shapes::add_consts(5, 3);
    a::none(
        &function,
        int_add(
            int_const(5u128).when_match(|_m, _ty, _b| false),
            int_const(3u128),
        )
        .into_pattern(),
    );
    a::matches(
        &function,
        int_add(
            int_const(5u128).when_match(|_m, _ty, _b| true),
            int_const(3u128),
        )
        .into_pattern(),
        1,
    );
}

#[test]
fn predicate_true_matches_all_outputs() {
    let function = shapes::add_consts(5, 3);
    let hits = Matcher::new(&function)
        .find_all(&predicate(|_m, _ty| true).into_pattern())
        .unwrap();
    assert!(!hits.is_empty());
}

#[test]
fn predicate_false_matches_nothing() {
    let function = shapes::add_consts(5, 3);
    a::matches(&function, predicate(|_m, _ty| false).into_pattern(), 0);
}

#[test]
fn predicate_inspects_node_kind() {
    let mut t = Tb::empty();
    let a_ = t.u64(7);
    let b_ = t.u64(3);
    let s = t.add(a_, b_);
    let function = t.ret_val(s);

    // The predicate signature only sees `(matcher, ty)`, so filtering on the
    // matched output means capturing it and reading the bindings in
    // `when_match`.
    let c = Capture::new();
    let hits = Matcher::new(&function)
        .find_all(
            &anything()
                .capture(c)
                .when_match(move |m, _ty, b| {
                    let Some(o) = b.get_value(c) else {
                        return false;
                    };
                    m.function().int_const_u128(o) == Some(7)
                })
                .into_pattern(),
        )
        .unwrap();
    assert_eq!(hits.len(), 1);
}

#[test]
fn capture_then_when_composes() {
    let function = shapes::add_consts(5, 3);
    let x = Capture::new();
    let hits = Matcher::new(&function)
        .find_all(
            &int_add(int_const(5u128), int_const(3u128))
                .capture(x)
                .when_match(|_m, _ty, _b| true)
                .into_pattern(),
        )
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert!(hits[0].value(x).is_some());
}

#[test]
fn get_int_const_returns_value() {
    let function = shapes::add_consts(5, 3);
    let x = Capture::new();
    assert_eq!(
        a::unique_uint(
            &function,
            int_add(var(x), int_const(3u128)).into_pattern(),
            x
        ),
        Some(5)
    );
}

#[test]
fn get_int_const_on_non_const_returns_none() {
    // `x` binds the `Add`, not a constant.
    let function = shapes::add_consts(5, 3);
    let x = Capture::new();
    assert_eq!(
        a::unique_uint(
            &function,
            int_add(int_const(5u128), int_const(3u128))
                .capture(x)
                .into_pattern(),
            x
        ),
        None,
    );
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
    // Return the I1 const directly: widening it would const-fold into a wider
    // `IntConst`, and `get_bool` only reads back an `IntConst` typed `I1`.
    let mut t = Tb::empty();
    let bc = t.boolean(true);
    let function = t.ret_val(bc);

    let v = Capture::new();
    let m = a::unique(&function, bool_const(true).capture(v).into_pattern());
    assert_eq!(m.bindings().get_bool(v, &function), Some(true));
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
    let m = a::unique(
        &function,
        int_add(int_const(5u128), int_const(3u128)).into_pattern(),
    );
    assert!(matches!(
        function.node_kind(m.root()),
        strider_ir::node::NodeKind::IntBinaryOp(strider_ir::IntBinaryOp::Add)
    ));
}

/// A parent's `filter` runs after the kind / output-type check but before the
/// matcher recurses into child sub-patterns, so a rejecting root filter must
/// leave the child's filter unfired.
#[test]
fn filter_short_circuits_before_child_recursion() {
    use std::cell::Cell;
    use std::rc::Rc;

    let function = shapes::add_consts(5, 7);

    let child_invocations: Rc<Cell<usize>> = Rc::new(Cell::new(0));
    let counter = child_invocations.clone();
    let child = anything().filter(move |_m, _n| {
        counter.set(counter.get() + 1);
        true
    });
    // Root filter always fails, before the child is walked.
    let root = int_add(int_const(99u128), child).filter(|_m, _n| false);

    a::none(&function, root.into_pattern());
    assert_eq!(
        child_invocations.get(),
        0,
        "child filter must NOT fire when the root filter short-circuits",
    );
}

/// Converse: an accepting root filter lets recursion reach the child filter.
#[test]
fn filter_accepts_match_and_visits_child() {
    use std::cell::Cell;
    use std::rc::Rc;

    let function = shapes::add_consts(5, 7);

    let child_invocations: Rc<Cell<usize>> = Rc::new(Cell::new(0));
    let counter = child_invocations.clone();
    let child = anything().filter(move |_m, _n| {
        counter.set(counter.get() + 1);
        true
    });
    let root = int_add(int_const(5u128), child).filter(|_m, _n| true);

    a::matches(&function, root.into_pattern(), 1);
    assert!(
        child_invocations.get() >= 1,
        "child filter fires once child recursion proceeds (got {})",
        child_invocations.get(),
    );
}
