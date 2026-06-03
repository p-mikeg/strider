//! `Matcher` API surface: `find_all`, `match_at`, `function_arg*`, walk-through
//! options, and the kind-prefilter early-out.

use strider_ir::IntBinaryOp;
use strider_ir::node::{NodeId, NodeKind, ValueType};
use strider_pattern::*;

use super::support::{Tb, assertions as a, shapes};

// ── Matcher::try_new / empty graphs ─────────────────────────────────────────

#[test]
fn new_on_empty_graph_does_not_panic() {
    // Empty function: just entry → return-nothing.
    let function = Tb::empty().ret_nothing();
    let _ = Matcher::try_new(&function).unwrap();
}

#[test]
fn find_all_on_empty_graph_returns_empty_for_specific_kind() {
    let function = Tb::empty().ret_nothing();
    a::none(&function, load().build());
    a::none(&function, call().build());
    a::none(&function, add(any(), any()).into_pattern());
}

// ── Kind-prefilter correctness ───────────────────────────────────────────────

#[test]
fn kind_prefilter_skips_incompatible_nodes() {
    // add(5, 3) has only IntConst + IntBinaryOp + Return + Entry.  load()
    // pattern is kind-filtered to Load, so it must return 0 matches without
    // attempting any structural work.
    let function = shapes::add_consts(5, 3);
    a::none(&function, load().build());
    a::none(&function, store().build());
    a::none(&function, call().build());
    a::none(&function, if_node().build());
    a::none(&function, phi().build());
}

// ── match_at ─────────────────────────────────────────────────────────────────

#[test]
fn match_at_hits_correct_node() {
    let function = shapes::add_consts(5, 3);
    let add_node = function
        .walk()
        .find(|&n| matches!(function.node_kind(n), NodeKind::IntBinaryOp(IntBinaryOp::Add)))
        .expect("add node exists");

    let pat = add(int_const(5u128), int_const(3u128)).into_pattern();
    let m = Matcher::try_new(&function)
        .unwrap()
        .match_at(add_node, &pat).unwrap()
        .expect("match_at should succeed on the Add node");
    assert_eq!(m.root(), add_node);
}

#[test]
fn match_at_on_wrong_kind_returns_none() {
    let function = shapes::add_consts(5, 3);
    let add_node = function
        .walk()
        .find(|&n| matches!(function.node_kind(n), NodeKind::IntBinaryOp(IntBinaryOp::Add)))
        .unwrap();
    let pat = load().build();
    let result = Matcher::try_new(&function).unwrap().match_at(add_node, &pat).unwrap();
    assert!(result.is_none());
}

#[test]
fn match_at_is_scoped_to_that_node_only() {
    let function = shapes::add_consts(5, 3);
    let add_node = function
        .walk()
        .find(|&n| matches!(function.node_kind(n), NodeKind::IntBinaryOp(IntBinaryOp::Add)))
        .unwrap();
    let pat = int_const(5u128).into_pattern();
    let result = Matcher::try_new(&function).unwrap().match_at(add_node, &pat).unwrap();
    assert!(result.is_none());
}

// ── find_all deterministic ordering ──────────────────────────────────────────

#[test]
fn find_all_is_deterministic() {
    let function = shapes::add_nested_3(1, 2, 3);
    let matcher = Matcher::try_new(&function).unwrap();
    let pat = any_int_const().capture(Capture::new()).into_pattern();

    let a1: Vec<_> = matcher.find_all(&pat).unwrap().into_iter().map(|m| m.root()).collect();
    let a2: Vec<_> = matcher.find_all(&pat).unwrap().into_iter().map(|m| m.root()).collect();

    assert_eq!(a1, a2);
}

// ── Lazy kind-index cache reuse across queries ───────────────────────────────

/// Run `find_all` with patterns at different discriminants against the
/// same matcher.  The lazy kind index is built on the first query and
/// reused on later ones; every query must still return its correct hit
/// count (regression check for the cached path).
#[test]
fn kind_index_reused_across_queries() {
    let function = shapes::add_consts(5, 7);
    let matcher = Matcher::try_new(&function).unwrap();

    // Query 1: any IntConst — two hits (5 and 7).
    let pat_const = any_int_const().into_pattern();
    assert_eq!(matcher.find_all(&pat_const).unwrap().len(), 2, "two IntConsts");

    // Query 2: Add(_, _) — one hit, served after the index is built.
    let pat_add = add(any(), any()).into_pattern();
    assert_eq!(matcher.find_all(&pat_add).unwrap().len(), 1, "one Add");

    // Query 3: re-run the IntConst query — still two hits via the cache.
    assert_eq!(matcher.find_all(&pat_const).unwrap().len(), 2, "re-query still two");
}

// ── function_arg*: empty graph ───────────────────────────────────────────────

#[test]
fn function_arg_apis_on_graph_without_args() {
    let function = shapes::add_consts(5, 3);
    let matcher = Matcher::try_new(&function).unwrap();

    assert!(matcher.function_arg(0).is_none());
    assert_eq!(matcher.function_args().count(), 0);
}

// ── Multi-match iteration & distinct bindings ────────────────────────────────

/// Graph with three distinct Add nodes at different operand values.
fn graph_three_adds() -> strider_ir::Function {
    let mut t = Tb::empty();
    let a = t.u64(1);
    let b = t.u64(2);
    let c = t.u64(3);
    let d = t.u64(4);
    let e = t.u64(5);
    let f = t.u64(6);
    let s1 = t.add(a, b);
    let s2 = t.add(c, d);
    let s3 = t.add(e, f);
    let s12 = t.add(s1, s2);
    let final_ = t.add(s12, s3);
    t.ret_val(final_)
}

#[test]
fn find_all_returns_distinct_matches_with_distinct_roots() {
    let function = graph_three_adds();
    let lhs = Capture::new();
    let rhs = Capture::new();
    let pat = add(any_int_const().capture(lhs), any_int_const().capture(rhs)).into_pattern();
    let hits = Matcher::try_new(&function).unwrap().find_all(&pat).unwrap();
    assert_eq!(hits.len(), 3);

    let roots: std::collections::HashSet<NodeId> = hits.iter().map(|m| m.root()).collect();
    assert_eq!(roots.len(), 3);
}

#[test]
fn each_match_has_its_own_bindings() {
    let function = graph_three_adds();
    let lhs = Capture::new();
    let rhs = Capture::new();
    let pat = add(any_int_const().capture(lhs), any_int_const().capture(rhs)).into_pattern();
    let hits = Matcher::try_new(&function).unwrap().find_all(&pat).unwrap();
    assert_eq!(hits.len(), 3);

    let mut got: Vec<(u128, u128)> = hits
        .iter()
        .map(|m| (m.bindings().get_uint(lhs, function.graph()).unwrap(), m.bindings().get_uint(rhs, function.graph()).unwrap()))
        .map(|(l, r)| if l < r { (l, r) } else { (r, l) })
        .collect();
    got.sort();
    assert_eq!(got, vec![(1, 2), (3, 4), (5, 6)]);
}

// ── Match::bindings_clone ────────────────────────────────────────────────────

#[test]
fn bindings_clone_outlives_match() {
    let function = shapes::add_consts(5, 3);
    let v = Capture::new();

    let bindings = {
        let matcher = Matcher::try_new(&function).unwrap();
        let node = function
            .walk()
            .find(|&n| matches!(function.node_kind(n), NodeKind::IntConst(5)))
            .unwrap();
        let pat = any_int_const().capture(v).into_pattern();
        let m = matcher.match_at(node, &pat).unwrap().expect("match");
        m.bindings_clone()
    };

    assert_eq!(bindings.get_uint(v, function.graph()), Some(5));
}

// ── Match::get_vn ────────────────────────────────────────────────────────────

#[test]
fn get_vn_on_initial_var_returns_varnode() {
    let (g, reg) = shapes::single_initial_var();
    let v = Capture::new();
    let m = a::unique(&g, initial_var().capture(v).into_pattern());
    assert_eq!(m.get_vn(v, &g), Some(reg));
}

#[test]
fn get_vn_on_non_mapped_producer_returns_none() {
    let function = shapes::add_consts(5, 3);
    let v = Capture::new();
    let m = a::unique(&function, add(int_const(5u128), int_const(3u128)).capture(v).into_pattern());
    assert_eq!(m.get_vn(v, &function), None);
}

#[test]
fn get_vn_on_unbound_var_returns_none() {
    let function = shapes::add_consts(5, 3);
    let m = a::first(&function, int_const(5u128).into_pattern());
    let never_bound = Capture::new();
    assert_eq!(m.get_vn(never_bound, &function), None);
}

// ── Default behaviour ───────────────────────────────────────────────────────

/// Regression: with both flags off, existing pattern queries return the
/// same matches as before.
#[test]
fn existing_pattern_unchanged_with_default_options() {
    let function = shapes::add_consts(5, 3);
    let pat = add(int_const(5u128), int_const(3u128)).into_pattern();
    let hits = Matcher::try_new(&function).unwrap().find_all(&pat).unwrap();
    assert_eq!(hits.len(), 1);
}

// ── ignore_casts walk-through ────────────────────────────────────────────────

/// Returns a graph whose return value is `Add(ZeroExt(Mul(2,3)), 4)` at I64,
/// where the Mul is at I32.
fn graph_add_zext_mul() -> strider_ir::Function {
    let mut t = Tb::empty();
    let two = t.u32(2);
    let three = t.u32(3);
    let mul = t.int_bin_at(two, three, IntBinaryOp::Mul, ValueType::I32);
    let widened = t.zext_to(mul, ValueType::I64);
    let four = t.u64(4);
    let total = t.add(widened, four);
    t.ret_val(total)
}

#[test]
fn add_mul_pattern_does_not_match_through_extend_by_default() {
    let function = graph_add_zext_mul();
    let pat = add(mul(any(), any()), any()).into_pattern();
    let hits = Matcher::try_new(&function).unwrap().find_all(&pat).unwrap();
    assert!(hits.is_empty());
}

#[test]
fn add_mul_pattern_matches_through_extend_with_ignore_casts() {
    let function = graph_add_zext_mul();
    // The cast mask now lives on the pattern, not the matcher.
    let pat = add(mul(any(), any()), any()).into_pattern().ignore_casts();
    let hits = Matcher::try_new(&function).unwrap().find_all(&pat).unwrap();
    assert_eq!(hits.len(), 1);
}

#[test]
fn add_mul_pattern_matches_through_chained_casts() {
    let function = {
        let mut t = Tb::empty();
        let two = t.u64(2);
        let three = t.u64(3);
        let mul = t.mul(two, three);
        let truncated = t.trunc_to(mul, ValueType::I32);
        let widened = t.zext_to(truncated, ValueType::I64);
        let four = t.u64(4);
        let total = t.add(widened, four);
        t.ret_val(total)
    };
    let pat = add(mul(any(), any()), any()).into_pattern().ignore_casts();
    let hits = Matcher::try_new(&function).unwrap().find_all(&pat).unwrap();
    assert_eq!(hits.len(), 1);
}

#[test]
fn truncate_pattern_still_matches_truncate_with_ignore_casts() {
    let function = {
        let mut t = Tb::empty();
        let a = t.u64(0xDEAD_BEEF);
        let b = t.u64(0x1234_5678);
        let or = t.bor(a, b);
        let truncated = t.trunc_to(or, ValueType::I32);
        t.ret_val(truncated)
    };
    let m = Matcher::try_new(&function).unwrap();
    let pat = truncate(any()).into_pattern().ignore_casts();
    let hits = m.find_all(&pat).unwrap();
    assert_eq!(hits.len(), 1);
    assert!(matches!(function.node_kind(hits[0].root()), NodeKind::Truncate));
}

#[test]
fn commutative_add_finds_mul_in_either_operand_through_extend() {
    let function = {
        let mut t = Tb::empty();
        let two = t.u32(2);
        let three = t.u32(3);
        let mul = t.int_bin_at(two, three, IntBinaryOp::Mul, ValueType::I32);
        let widened = t.zext_to(mul, ValueType::I64);
        let four = t.u64(4);
        let total = t.add(four, widened);
        t.ret_val(total)
    };
    let pat = add(mul(any(), any()), any()).into_pattern().ignore_casts();
    let hits = Matcher::try_new(&function).unwrap().find_all(&pat).unwrap();
    assert_eq!(hits.len(), 1);
}

// ── Region boundaries are explicit ───────────────────────────────────

/// Two-region graph: entry region runs `Call`; tail region runs `Return`.
fn graph_ret_via_region_after_call() -> strider_ir::Function {
    let mut t = Tb::bare(vec![], &[], &[], &[], None, 0);
    let head = t.fb_mut().create_region().expect("head");
    t.fb_mut().set_entry_region(head).expect("entry head");
    t.fb_mut().set_region(head);

    let target = t
        .fb_mut()
        .build_int_const(0xCAFEu64, ValueType::I64)
        .unwrap();
    t.fb_mut().build_call(target, None).expect("call");

    let tail = t.fb_mut().create_region().expect("tail");
    t.fb_mut().build_branch(tail).expect("branch to tail");

    t.fb_mut().set_region(tail);
    t.fb_mut().build_return(None, &[]).expect("ret");

    t.finish()
}

#[test]
fn ret_call_does_not_match_through_region_by_default() {
    let function = graph_ret_via_region_after_call();
    // "Return preceded by a Call node": express the Call-kind predecessor
    // via a node-level filter (the new API has no Call-kind value wildcard,
    // and `preceded_by` only inspects the direct ctrl predecessor).  The
    // Return's ctrl predecessor is the tail Region, not the Call, so this
    // must NOT match — the region boundary is explicit.
    let pat = ret()
        .preceded_by(any().filter(|m, node| {
            matches!(m.function().node_kind(node), strider_ir::node::NodeKind::Call)
        }))
        .build();
    let hits = Matcher::try_new(&function).unwrap().find_all(&pat).unwrap();
    assert!(hits.is_empty());
}

// ── find_first: short-circuiting single-match query ──────────────────────────

#[test]
fn find_first_concrete_kind_equals_find_all_first() {
    let function = shapes::add_nested_3(2, 3, 5);
    let m = Matcher::try_new(&function).unwrap();
    let p = add(any(), any()).into_pattern();
    let all = m.find_all(&p).unwrap();
    assert!(!all.is_empty(), "fixture has Add nodes");
    let first = m.find_first(&p).unwrap().expect("at least one Add matches");
    assert_eq!(first.root(), all[0].root());
}

#[test]
fn find_first_wildcard_equals_find_all_first() {
    let function = shapes::add_consts(1, 2);
    let m = Matcher::try_new(&function).unwrap();
    let p = any().into_pattern();
    let all = m.find_all(&p).unwrap();
    assert!(!all.is_empty());
    let first = m.find_first(&p).unwrap().expect("wildcard matches some node");
    assert_eq!(first.root(), all[0].root());
}

#[test]
fn find_first_no_match_returns_none() {
    let function = shapes::add_consts(5, 3);
    let m = Matcher::try_new(&function).unwrap();
    // No Load node in an arithmetic-only graph.
    assert!(m.find_first(&load().build()).unwrap().is_none());
    assert!(m.find_first(&call().build()).unwrap().is_none());
}

// ── find_joined: shared-capture cross-pattern intersection ──────────

#[test]
fn find_joined_empty_input() {
    let function = shapes::add_consts(2, 3);
    let m = Matcher::try_new(&function).unwrap();
    let results = m.find_joined(&[]).unwrap();
    assert!(results.is_empty());
}

#[test]
fn find_joined_single_pattern_equivalent_to_find_all() {
    let function = shapes::add_consts(2, 3);
    let m = Matcher::try_new(&function).unwrap();
    let p = add(any(), any()).into_pattern();
    let req = m.find_joined(&[&p]).unwrap();
    let direct = m.find_all(&p).unwrap();
    assert_eq!(req.len(), direct.len());
    for (mr, dr) in req.iter().zip(direct.iter()) {
        assert_eq!(mr.len(), 1);
        assert_eq!(mr[0].root(), dr.root());
    }
}

#[test]
fn find_joined_no_matches_for_a_pattern_yields_empty() {
    let function = shapes::add_consts(2, 3);
    let m = Matcher::try_new(&function).unwrap();
    let p_add = add(any(), any()).into_pattern();
    let p_call = call().build();
    let req = m.find_joined(&[&p_add, &p_call]).unwrap();
    assert!(req.is_empty());
}

#[test]
fn find_joined_intersects_on_shared_capture_node_id() {
    let mut t = Tb::empty();
    let a = t.u64(0xAAAA);
    let b = t.u64(0xBBBB);
    let off8 = t.u64(8);
    let off16 = t.u64(16);
    let off24 = t.u64(24);
    let zero = t.u64(0);
    let v99 = t.u64(99);
    let addr_a8 = t.add(a, off8);
    let addr_b16 = t.add(b, off16);
    let addr_a24 = t.add(a, off24);
    t.store_ram(addr_a8, zero);
    t.store_ram(addr_b16, zero);
    t.store_ram(addr_a24, v99);
    let function = t.ret_nothing();

    let mr = Matcher::try_new(&function).unwrap();
    let shared = Capture::new();
    let k = Capture::new();
    let p_zero = store()
        .addr(add(var(shared), any_int_const().capture(k)).ordered())
        .data(int_const(0u128))
        .build();
    let p_99 = store()
        .addr(add(var(shared), any_int_const().capture(Capture::new())).ordered())
        .data(int_const(99u128))
        .build();

    let req = mr.find_joined(&[&p_zero, &p_99]).unwrap();
    assert_eq!(req.len(), 1);
    let inner = &req[0];
    assert_eq!(inner.len(), 2);

    let s1 = inner[0].node(shared, function.graph()).expect("shared bound in pat[0]");
    let s2 = inner[1].node(shared, function.graph()).expect("shared bound in pat[1]");
    assert_eq!(s1, s2);

    let k_val = inner[0].bindings().get_uint(k, function.graph()).expect("K bound");
    assert_eq!(k_val, 8);
}

#[test]
fn find_joined_disagreement_on_shared_capture_yields_empty() {
    let mut t = Tb::empty();
    let a = t.u64(0xAAAA);
    let b = t.u64(0xBBBB);
    let off8 = t.u64(8);
    let off16 = t.u64(16);
    let zero = t.u64(0);
    let addr_a8 = t.add(a, off8);
    let addr_b16 = t.add(b, off16);
    t.store_ram(addr_a8, zero);
    t.store_ram(addr_b16, zero);
    let function = t.ret_nothing();

    let mr = Matcher::try_new(&function).unwrap();
    let shared = Capture::new();
    let p_8 = store()
        .addr(add(var(shared), int_const(8u128)).ordered())
        .data(int_const(0u128))
        .build();
    let p_16 = store()
        .addr(add(var(shared), int_const(16u128)).ordered())
        .data(int_const(0u128))
        .build();
    let req = mr.find_joined(&[&p_8, &p_16]).unwrap();
    assert!(req.is_empty());
}
