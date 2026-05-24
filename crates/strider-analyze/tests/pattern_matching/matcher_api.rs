//! `Matcher` API surface: `find_all`, `match_at`, `function_arg*`, walk-through
//! options, and the kind-prefilter early-out.

use strider_analyze::pattern::*;
use strider_ir::IntBinaryOp;
use strider_ir::node::{NodeId, NodeKind, NodeOutputType};

use super::support::{Tb, assertions as a, shapes};

// ── Matcher::try_new / empty graphs ─────────────────────────────────────────

#[test]
fn new_on_empty_graph_does_not_panic() {
    // Empty function: just entry → return-nothing.
    let g = Tb::empty().ret_nothing();
    let _ = Matcher::try_new(&g).unwrap();
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
    let add_node = g
        .preorder()
        .find(|&n| matches!(g.node_kind(n), NodeKind::IntBinaryOp(IntBinaryOp::Add)))
        .expect("add node exists");

    let pat: Pat = add(int_const(5), int_const(3)).into();
    let m = Matcher::try_new(&g).unwrap()
        .match_at(add_node, &pat)
        .expect("match_at should succeed on the Add node");
    assert_eq!(m.root(), add_node);
}

#[test]
fn match_at_on_wrong_kind_returns_none() {
    let g = shapes::add_consts(5, 3);
    let add_node = g
        .preorder()
        .find(|&n| matches!(g.node_kind(n), NodeKind::IntBinaryOp(IntBinaryOp::Add)))
        .unwrap();
    let pat: Pat = load().into();
    let result = Matcher::try_new(&g).unwrap().match_at(add_node, &pat);
    assert!(result.is_none());
}

#[test]
fn match_at_is_scoped_to_that_node_only() {
    let g = shapes::add_consts(5, 3);
    let add_node = g
        .preorder()
        .find(|&n| matches!(g.node_kind(n), NodeKind::IntBinaryOp(IntBinaryOp::Add)))
        .unwrap();
    let pat: Pat = int_const(5);
    let result = Matcher::try_new(&g).unwrap().match_at(add_node, &pat);
    assert!(result.is_none());
}

// ── find_all deterministic ordering ──────────────────────────────────────────

#[test]
fn find_all_is_deterministic() {
    let g = shapes::add_nested_3(1, 2, 3);
    let matcher = Matcher::try_new(&g).unwrap();
    let pat: Pat = any_int_const(Capture::new());

    let a1: Vec<_> = matcher.find_all(&pat).into_iter().map(|m| m.root()).collect();
    let a2: Vec<_> = matcher.find_all(&pat).into_iter().map(|m| m.root()).collect();

    assert_eq!(a1, a2);
}

// ── function_arg*: empty graph ───────────────────────────────────────────────

#[test]
fn function_arg_apis_on_graph_without_args() {
    let g = shapes::add_consts(5, 3);
    let matcher = Matcher::try_new(&g).unwrap();

    assert!(matcher.function_arg(0).is_none());
    assert_eq!(matcher.function_arg_count(), 0);
    assert_eq!(matcher.function_arg_len(), 0);
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
    let g = graph_three_adds();
    let lhs = Capture::new();
    let rhs = Capture::new();
    let pat: Pat = add(any_int_const(lhs), any_int_const(rhs)).into();
    let hits = Matcher::try_new(&g).unwrap().find_all(&pat);
    assert_eq!(hits.len(), 3);

    let roots: std::collections::HashSet<NodeId> = hits.iter().map(|m| m.root()).collect();
    assert_eq!(roots.len(), 3);
}

#[test]
fn each_match_has_its_own_bindings() {
    let g = graph_three_adds();
    let lhs = Capture::new();
    let rhs = Capture::new();
    let pat: Pat = add(any_int_const(lhs), any_int_const(rhs)).into();
    let hits = Matcher::try_new(&g).unwrap().find_all(&pat);
    assert_eq!(hits.len(), 3);

    let mut got: Vec<(u128, u128)> = hits
        .iter()
        .map(|m| (m.get_uint(lhs, &g).unwrap(), m.get_uint(rhs, &g).unwrap()))
        .map(|(l, r)| if l < r { (l, r) } else { (r, l) })
        .collect();
    got.sort();
    assert_eq!(got, vec![(1, 2), (3, 4), (5, 6)]);
}

// ── Match::bindings_clone ────────────────────────────────────────────────────

#[test]
fn bindings_clone_outlives_match() {
    let g = shapes::add_consts(5, 3);
    let v = Capture::new();

    let bindings = {
        let matcher = Matcher::try_new(&g).unwrap();
        let node = g
            .preorder()
            .find(|&n| matches!(g.node_kind(n), NodeKind::IntConst(5)))
            .unwrap();
        let m = matcher
            .match_at(node, &any_int_const(v))
            .expect("match");
        m.bindings_clone()
    };

    assert_eq!(bindings.get_uint(v, &g), Some(5));
}

// ── Match::get_vn ────────────────────────────────────────────────────────────

#[test]
fn get_vn_on_initial_var_returns_varnode() {
    let (g, reg) = shapes::single_initial_var();
    let v = Capture::new();
    let m = a::unique(&g, initial_var().capture(v));
    assert_eq!(m.get_vn(v, &g), Some(reg));
}

#[test]
fn get_vn_on_non_mapped_producer_returns_none() {
    let g = shapes::add_consts(5, 3);
    let v = Capture::new();
    let m = a::unique(&g, add(int_const(5), int_const(3)).capture(v));
    assert_eq!(m.get_vn(v, &g), None);
}

#[test]
fn get_vn_on_call_ret_output_returns_ret_reg() {
    let ret = super::support::reg_vn(0, 8);
    let mut t = Tb::raw(vec![ret], &[], &[], &[ret], None, 0);
    t.call_at(0xCAFE);
    let g = t.ret_regs(&[ret]);

    let v = Capture::new();
    let m = a::unique(&g, call().at(0xCAFE).ret_output(0, var(v)));
    assert_eq!(m.get_vn(v, &g), Some(ret));
}

#[test]
fn get_vn_on_unbound_var_returns_none() {
    let g = shapes::add_consts(5, 3);
    let m = a::first(&g, int_const(5));
    let never_bound = Capture::new();
    assert_eq!(m.get_vn(never_bound, &g), None);
}

// ── Default behaviour ───────────────────────────────────────────────────────

/// Regression: with both flags off, existing pattern queries return the
/// same matches as before.
#[test]
fn existing_pattern_unchanged_with_default_options() {
    let g = shapes::add_consts(5, 3);
    let pat: Pat = add(int_const(5), int_const(3)).into();
    let hits = Matcher::try_new(&g).unwrap().find_all(&pat);
    assert_eq!(hits.len(), 1);
}

// ── ignore_casts walk-through ────────────────────────────────────────────────

/// Returns a graph whose return value is `Add(ZeroExt(Mul(2,3)), 4)` at U64,
/// where the Mul is at U32.
fn graph_add_zext_mul() -> strider_ir::Function {
    let mut t = Tb::empty();
    let two = t.u32(2);
    let three = t.u32(3);
    let mul = t.int_bin_at(two, three, IntBinaryOp::Mul, NodeOutputType::U32);
    let widened = t.zext_to(mul, NodeOutputType::U64);
    let four = t.u64(4);
    let total = t.add(widened, four);
    t.ret_val(total)
}

#[test]
fn add_mul_pattern_does_not_match_through_extend_by_default() {
    let g = graph_add_zext_mul();
    let pat: Pat = add(mul(any(), any()), any()).into();
    let hits = Matcher::try_new(&g).unwrap().find_all(&pat);
    assert!(hits.is_empty());
}

#[test]
fn add_mul_pattern_matches_through_extend_with_ignore_casts() {
    let g = graph_add_zext_mul();
    let pat: Pat = add(mul(any(), any()), any()).into();
    let hits = Matcher::try_new(&g).unwrap().ignore_casts().find_all(&pat);
    assert_eq!(hits.len(), 1);
}

#[test]
fn add_mul_pattern_matches_through_chained_casts() {
    let g = {
        let mut t = Tb::empty();
        let two = t.u64(2);
        let three = t.u64(3);
        let mul = t.mul(two, three);
        let truncated = t.trunc_to(mul, NodeOutputType::U32);
        let widened = t.zext_to(truncated, NodeOutputType::U64);
        let four = t.u64(4);
        let total = t.add(widened, four);
        t.ret_val(total)
    };
    let pat: Pat = add(mul(any(), any()), any()).into();
    let hits = Matcher::try_new(&g).unwrap().ignore_casts().find_all(&pat);
    assert_eq!(hits.len(), 1);
}

#[test]
fn truncate_pattern_still_matches_truncate_with_ignore_casts() {
    let g = {
        let mut t = Tb::empty();
        let a = t.u64(0xDEAD_BEEF);
        let b = t.u64(0x1234_5678);
        let or = t.bor(a, b);
        let truncated = t.trunc_to(or, NodeOutputType::U32);
        t.ret_val(truncated)
    };
    let m = Matcher::try_new(&g).unwrap().ignore_casts();
    let hits = m.find_all(&truncate(any()));
    assert_eq!(hits.len(), 1);
    assert!(matches!(g.node_kind(hits[0].root()), NodeKind::Truncate));
}

#[test]
fn commutative_add_finds_mul_in_either_operand_through_extend() {
    let g = {
        let mut t = Tb::empty();
        let two = t.u32(2);
        let three = t.u32(3);
        let mul = t.int_bin_at(two, three, IntBinaryOp::Mul, NodeOutputType::U32);
        let widened = t.zext_to(mul, NodeOutputType::U64);
        let four = t.u64(4);
        let total = t.add(four, widened);
        t.ret_val(total)
    };
    let pat: Pat = add(mul(any(), any()), any()).into();
    let hits = Matcher::try_new(&g).unwrap().ignore_casts().find_all(&pat);
    assert_eq!(hits.len(), 1);
}

// ── ignore_regions walk-through ──────────────────────────────────────

/// Two-region graph: entry region runs `Call`; tail region runs `Return`.
fn graph_ret_via_region_after_call() -> strider_ir::Function {
    let mut t = Tb::bare(vec![], &[], &[], &[], None, 0);
    let head = t.fb_mut().create_region().expect("head");
    t.fb_mut().set_entry_region(head).expect("entry head");
    t.fb_mut().set_region(head);

    let target = t
        .fb_mut()
        .build_int_const(0xCAFEu64, NodeOutputType::U64)
        .unwrap();
    t.fb_mut().build_call(target).expect("call");

    let tail = t.fb_mut().create_region().expect("tail");
    t.fb_mut().build_branch(tail).expect("branch to tail");

    t.fb_mut().set_region(tail);
    t.fb_mut().build_return(None, &[]).expect("ret");

    t.finish()
}

#[test]
fn ret_call_does_not_match_through_region_by_default() {
    let g = graph_ret_via_region_after_call();
    let pat: Pat = ret().preceded_by(call()).into();
    let hits = Matcher::try_new(&g).unwrap().find_all(&pat);
    assert!(hits.is_empty());
}

#[test]
fn ret_call_matches_through_controlstate_with_ignore_regions() {
    let g = graph_ret_via_region_after_call();
    let pat: Pat = ret().preceded_by(call()).into();
    let hits = Matcher::try_new(&g).unwrap().ignore_regions().find_all(&pat);
    assert_eq!(hits.len(), 1);
}

#[test]
fn both_flags_together_do_not_interfere_with_value_walk_through() {
    let g = graph_add_zext_mul();
    let pat: Pat = add(mul(any(), any()), any()).into();
    let hits = Matcher::try_new(&g).unwrap()
        .ignore_casts()
        .ignore_regions()
        .find_all(&pat);
    assert_eq!(hits.len(), 1);
}

// ── find_all_multi: equivalence with sequential find_all ─────────────────────

#[test]
fn find_all_multi_matches_sequential_find_all() {
    let g = shapes::add_nested_3(5, 7, 11);
    let m = Matcher::try_new(&g).unwrap();

    let p_add: Pat = add(any(), any()).into();
    let p_const: Pat = any_int_const(Capture::new());
    let p_load: Pat = load().into();

    let multi = m.find_all_multi(&[&p_add, &p_const, &p_load]);

    let seq_add = m.find_all(&p_add);
    let seq_const = m.find_all(&p_const);
    let seq_load = m.find_all(&p_load);

    let roots = |hits: &[Match]| hits.iter().map(|h| h.root()).collect::<Vec<_>>();
    assert_eq!(roots(&multi[0]), roots(&seq_add));
    assert_eq!(roots(&multi[1]), roots(&seq_const));
    assert_eq!(roots(&multi[2]), roots(&seq_load));
}

#[test]
fn find_all_multi_empty_input() {
    let g = shapes::add_consts(2, 3);
    let m = Matcher::try_new(&g).unwrap();
    let results = m.find_all_multi(&[]);
    assert!(results.is_empty());
}

#[test]
fn find_all_multi_all_wildcards() {
    let g = shapes::add_consts(1, 2);
    let m = Matcher::try_new(&g).unwrap();
    let p1: Pat = any();
    let p2: Pat = any();
    let multi = m.find_all_multi(&[&p1, &p2]);
    assert_eq!(multi[0].len(), m.find_all(&p1).len());
    assert_eq!(multi[1].len(), m.find_all(&p2).len());
}

#[test]
fn find_all_multi_mixed_concrete_and_wildcard() {
    let g = shapes::add_nested_3(2, 3, 5);
    let m = Matcher::try_new(&g).unwrap();
    let p_add: Pat = add(any(), any()).into();
    let p_wild: Pat = any();
    let multi = m.find_all_multi(&[&p_add, &p_wild]);
    let roots = |hits: &[Match]| hits.iter().map(|h| h.root()).collect::<Vec<_>>();
    assert_eq!(roots(&multi[0]), roots(&m.find_all(&p_add)));
    assert_eq!(roots(&multi[1]), roots(&m.find_all(&p_wild)));
}

// ── find_all_requirements: shared-capture cross-pattern intersection ──────────

#[test]
fn find_all_requirements_empty_input() {
    let g = shapes::add_consts(2, 3);
    let m = Matcher::try_new(&g).unwrap();
    let results = m.find_all_requirements(&[]);
    assert!(results.is_empty());
}

#[test]
fn find_all_requirements_single_pattern_equivalent_to_find_all() {
    let g = shapes::add_consts(2, 3);
    let m = Matcher::try_new(&g).unwrap();
    let p: Pat = add(any(), any()).into();
    let req = m.find_all_requirements(&[&p]);
    let direct = m.find_all(&p);
    assert_eq!(req.len(), direct.len());
    for (mr, dr) in req.iter().zip(direct.iter()) {
        assert_eq!(mr.len(), 1);
        assert_eq!(mr[0].root(), dr.root());
    }
}

#[test]
fn find_all_requirements_no_matches_for_a_pattern_yields_empty() {
    let g = shapes::add_consts(2, 3);
    let m = Matcher::try_new(&g).unwrap();
    let p_add: Pat = add(any(), any()).into();
    let p_call: Pat = call().into();
    let req = m.find_all_requirements(&[&p_add, &p_call]);
    assert!(req.is_empty());
}

#[test]
fn find_all_requirements_intersects_on_shared_capture_node_id() {
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
    let g = t.ret_nothing();

    let mr = Matcher::try_new(&g).unwrap();
    let shared = Capture::new();
    let k = Capture::new();
    let p_zero: Pat = store()
        .addr(add(var(shared), any_int_const(k)).ordered())
        .data(int_const(0))
        .into();
    let p_99: Pat = store()
        .addr(add(var(shared), any_int_const(Capture::new())).ordered())
        .data(int_const(99))
        .into();

    let req = mr.find_all_requirements(&[&p_zero, &p_99]);
    assert_eq!(req.len(), 1);
    let inner = &req[0];
    assert_eq!(inner.len(), 2);

    let s1 = inner[0].node(shared).expect("shared bound in pat[0]");
    let s2 = inner[1].node(shared).expect("shared bound in pat[1]");
    assert_eq!(s1, s2);

    let k_val = inner[0].get_uint(k, &g).expect("K bound");
    assert_eq!(k_val, 8);
}

#[test]
fn find_all_requirements_disagreement_on_shared_capture_yields_empty() {
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
    let g = t.ret_nothing();

    let mr = Matcher::try_new(&g).unwrap();
    let shared = Capture::new();
    let p_8: Pat = store()
        .addr(add(var(shared), int_const(8u64)).ordered())
        .data(int_const(0))
        .into();
    let p_16: Pat = store()
        .addr(add(var(shared), int_const(16u64)).ordered())
        .data(int_const(0))
        .into();
    let req = mr.find_all_requirements(&[&p_8, &p_16]);
    assert!(req.is_empty());
}
