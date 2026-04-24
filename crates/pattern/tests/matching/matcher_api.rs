//! `Matcher` API surface: `find_all`, `match_at`, `function_arg*`, and the
//! kind-prefilter early-out.

use ir::IntBinaryOp;
use ir::node::{NodeId, NodeKind};
use pattern::*;

use super::support::{Tb, assertions as a, shapes};

// ── Matcher::new / empty graphs ──────────────────────────────────────────────

#[test]
fn new_on_empty_graph_does_not_panic() {
    // Empty function: just entry → return-nothing.
    let g = Tb::empty().ret_nothing();
    let _ = Matcher::new(&g);
}

#[test]
fn find_all_on_empty_graph_returns_empty_for_specific_kind() {
    let g = Tb::empty().ret_nothing();
    a::none(&g, load());
    a::none(&g, call());
    a::none(&g, add(any(), any()));
}

// ── Kind-prefilter correctness ───────────────────────────────────────────────

#[test]
fn kind_prefilter_skips_incompatible_nodes() {
    // add(5, 3) has only IntConst + IntBinaryOp + Return + Entry.  load()
    // pattern is kind-filtered to Load, so it must return 0 matches without
    // attempting any structural work.
    let g = shapes::add_consts(5, 3);
    a::none(&g, load());
    a::none(&g, store());
    a::none(&g, call());
    a::none(&g, if_node());
    a::none(&g, phi());
}

// ── match_at ─────────────────────────────────────────────────────────────────

#[test]
fn match_at_hits_correct_node() {
    let g = shapes::add_consts(5, 3);
    // Find the Add node directly.
    let add_node = g
        .preorder()
        .find(|&n| matches!(g.graph.node_kind(n), NodeKind::IntBinaryOp(IntBinaryOp::Add)))
        .expect("add node exists");

    let m = Matcher::new(&g)
        .match_at(add_node, &add(int_const(5), int_const(3)).into())
        .expect("match_at should succeed on the Add node");
    assert_eq!(m.root, add_node);
}

#[test]
fn match_at_on_wrong_kind_returns_none() {
    let g = shapes::add_consts(5, 3);
    // Apply `load()` at the Add node — kind mismatch → None, no panic.
    let add_node = g
        .preorder()
        .find(|&n| matches!(g.graph.node_kind(n), NodeKind::IntBinaryOp(IntBinaryOp::Add)))
        .unwrap();

    let result = Matcher::new(&g).match_at(add_node, &load().into());
    assert!(result.is_none());
}

#[test]
fn match_at_is_scoped_to_that_node_only() {
    // Graph has two IntConsts (5 and 3) and one Add.
    let g = shapes::add_consts(5, 3);
    let add_node = g
        .preorder()
        .find(|&n| matches!(g.graph.node_kind(n), NodeKind::IntBinaryOp(IntBinaryOp::Add)))
        .unwrap();

    // Ask `int_const(5)` at the Add node: the pattern is for IntConst, Add
    // has a different kind → None.
    let result = Matcher::new(&g).match_at(add_node, &int_const(5));
    assert!(result.is_none());
}

// ── find_all deterministic ordering ──────────────────────────────────────────

#[test]
fn find_all_is_deterministic() {
    let g = shapes::add_nested_3(1, 2, 3);
    let matcher = Matcher::new(&g);
    let pat: Pat = any_int_const(IntVar::new());

    let a1: Vec<_> = matcher.find_all(&pat).into_iter().map(|m| m.root).collect();
    let a2: Vec<_> = matcher.find_all(&pat).into_iter().map(|m| m.root).collect();

    assert_eq!(a1, a2);
}

// ── function_arg*: empty graph ───────────────────────────────────────────────

#[test]
fn function_arg_apis_on_graph_without_args() {
    let g = shapes::add_consts(5, 3);
    let matcher = Matcher::new(&g);

    assert!(matcher.function_arg(0).is_none());
    assert_eq!(matcher.function_arg_count(), 0);
    assert_eq!(matcher.function_arg_len(), 0);
    assert_eq!(matcher.function_args().count(), 0);
}

// ── Multi-match iteration & distinct bindings ────────────────────────────────

/// Graph with three distinct Add nodes at different operand values; the
/// `add(any_int_const, any_int_const)` pattern should find all three and
/// each `Match` should carry its own captures.
fn graph_three_adds() -> ir::BuiltFunctionGraph {
    let mut t = Tb::empty();
    let a = t.u64(1);
    let b = t.u64(2);
    let c = t.u64(3);
    let d = t.u64(4);
    let e = t.u64(5);
    let f = t.u64(6);
    let s1 = t.add(a, b); // add(1, 2)
    let s2 = t.add(c, d); // add(3, 4)
    let s3 = t.add(e, f); // add(5, 6)
    // Thread all three into the Return via an outer Add so none are dead.
    let s12 = t.add(s1, s2);
    let final_ = t.add(s12, s3);
    t.ret_val(final_)
}

#[test]
fn find_all_returns_distinct_matches_with_distinct_roots() {
    let g = graph_three_adds();
    let lhs = IntVar::new();
    let rhs = IntVar::new();
    let hits = Matcher::new(&g).find_all(&add(any_int_const(lhs), any_int_const(rhs)).into());
    // Three leaf Adds + two outer Adds (the outer ones have non-const
    // operands, so they DON'T match `any_int_const × any_int_const`).
    assert_eq!(hits.len(), 3);

    // All three `root` NodeIds are distinct.
    let roots: std::collections::HashSet<NodeId> = hits.iter().map(|m| m.root).collect();
    assert_eq!(roots.len(), 3);
}

#[test]
fn each_match_has_its_own_bindings() {
    let g = graph_three_adds();
    let lhs = IntVar::new();
    let rhs = IntVar::new();
    let hits = Matcher::new(&g).find_all(&add(any_int_const(lhs), any_int_const(rhs)).into());
    assert_eq!(hits.len(), 3);

    // Gather the (lhs, rhs) captures across all matches as an order-
    // independent set.  Exactly the three operand pairs must appear.
    let mut got: Vec<(u64, u64)> = hits
        .iter()
        .map(|m| (m.get_int(lhs).unwrap(), m.get_int(rhs).unwrap()))
        .map(|(l, r)| if l < r { (l, r) } else { (r, l) }) // commutative retry can swap
        .collect();
    got.sort();
    assert_eq!(got, vec![(1, 2), (3, 4), (5, 6)]);
}

// ── Match::bindings_clone ────────────────────────────────────────────────────

#[test]
fn bindings_clone_outlives_match() {
    let g = shapes::add_consts(5, 3);
    let v = IntVar::new();

    // Clone the bindings, then drop the Match.  The snapshot still resolves.
    let bindings = {
        let matcher = Matcher::new(&g);
        let m = matcher
            .match_at(
                g.preorder()
                    .find(|&n| matches!(g.graph.node_kind(n), NodeKind::IntConst(5)))
                    .unwrap(),
                &any_int_const(v),
            )
            .expect("match");
        m.bindings_clone()
    };

    assert_eq!(bindings.get_int(v), Some(5));
}

// ── Match::get_vn ────────────────────────────────────────────────────────────

#[test]
fn get_vn_on_initial_var_returns_varnode() {
    let (g, reg) = shapes::single_initial_var();
    let v = Var::new();
    let m = a::unique(&g, initial_var().capture(v));
    assert_eq!(m.get_vn(v, &g), Some(reg));
}

#[test]
fn get_vn_on_non_mapped_producer_returns_none() {
    let g = shapes::add_consts(5, 3);
    let v = Var::new();
    // Capture the Add itself — `get_vn` only has a meaning for InitialVar
    // and Call ret-output slots, so this must return None.
    let m = a::unique(&g, add(int_const(5), int_const(3)).capture(v));
    assert_eq!(m.get_vn(v, &g), None);
}

#[test]
fn get_vn_on_call_ret_output_returns_ret_reg() {
    // Single-ret-reg ABI stub; Call output slot 2 represents the post-call
    // value of `ret`.
    let ret = super::support::reg_vn(0, 8);
    let mut t = Tb::raw(vec![ret], &[], &[], &[ret], None, 0);
    t.call_at(0xCAFE);
    let g = t.ret_regs(&[ret]);

    let v = Var::new();
    let m = a::unique(&g, call().at(0xCAFE).ret_output(0, var(v)));
    assert_eq!(m.get_vn(v, &g), Some(ret));
}

#[test]
fn get_vn_on_unbound_var_returns_none() {
    let g = shapes::add_consts(5, 3);
    let m = a::first(&g, int_const(5));
    let never_bound = Var::new();
    assert_eq!(m.get_vn(never_bound, &g), None);
}
