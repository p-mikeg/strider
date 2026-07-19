//! `Matcher` API surface: `find_all`, `match_at`, `function_arg*`, walk-through
//! options, and the kind-prefilter early-out.

use strider_ir::node::{NodeId, NodeKind, ValueType};
use strider_ir::{IRBuilderExt, IRViewer, IRWalker, IntBinaryOp};
use strider_pattern::*;

use super::support::{Tb, assertions as a, shapes};

#[test]
fn new_on_empty_graph_does_not_panic() {
    let function = Tb::empty().ret_nothing();
    let _ = Matcher::new(&function);
}

#[test]
fn find_all_on_empty_graph_returns_empty_for_specific_kind() {
    let function = Tb::empty().ret_nothing();
    a::none(&function, load().build());
    a::none(&function, call().build());
    a::none(&function, add(any(), any()).into_pattern());
}

/// The fixture holds only IntConst / IntBinaryOp / Return / Entry, so each
/// kind-filtered pattern must bail before any structural work.
#[test]
fn kind_prefilter_skips_incompatible_nodes() {
    let function = shapes::add_consts(5, 3);
    a::none(&function, load().build());
    a::none(&function, store().build());
    a::none(&function, call().build());
    a::none(&function, if_node().build());
    a::none(&function, phi().build());
}

#[test]
fn match_at_hits_correct_node() {
    let function = shapes::add_consts(5, 3);
    let add_node = function
        .walk()
        .find(|&n| {
            matches!(
                function.node_kind(n),
                NodeKind::IntBinaryOp(IntBinaryOp::Add)
            )
        })
        .expect("add node exists");

    let pat = add(int_const(5u128), int_const(3u128)).into_pattern();
    let m = Matcher::new(&function)
        .match_at(add_node, &pat)
        .unwrap()
        .expect("match_at should succeed on the Add node");
    assert_eq!(m.root(), add_node);
}

/// The footprint is root + interior + captured leaves, not just the captures.
/// The rewrite engine absorbs fingerprints from it, and a backward-BFS
/// reconstruction would miss multi-sink / non-cone matched nodes.
#[test]
fn matched_nodes_includes_root_and_all_structural_operands() {
    let function = shapes::add_consts(5, 3);
    let add_node = function
        .walk()
        .find(|&n| {
            matches!(
                function.node_kind(n),
                NodeKind::IntBinaryOp(IntBinaryOp::Add)
            )
        })
        .expect("add node exists");
    let const_nodes: Vec<NodeId> = function
        .walk()
        .filter(|&n| matches!(function.node_kind(n), NodeKind::IntConst(_)))
        .collect();
    assert_eq!(const_nodes.len(), 2, "two operand consts");

    let pat = add(int_const(5u128), int_const(3u128)).into_pattern();
    let m = Matcher::new(&function)
        .match_at(add_node, &pat)
        .unwrap()
        .expect("match_at should succeed");

    let matched = m.matched_nodes();
    assert!(
        matched.contains(&add_node),
        "root Add is in the matched footprint"
    );
    for c in &const_nodes {
        assert!(
            matched.contains(c),
            "operand const {c:?} is in the matched footprint",
        );
    }
}

#[test]
fn match_at_on_wrong_kind_returns_none() {
    let function = shapes::add_consts(5, 3);
    let add_node = function
        .walk()
        .find(|&n| {
            matches!(
                function.node_kind(n),
                NodeKind::IntBinaryOp(IntBinaryOp::Add)
            )
        })
        .unwrap();
    let pat = load().build();
    let result = Matcher::new(&function).match_at(add_node, &pat).unwrap();
    assert!(result.is_none());
}

#[test]
fn match_at_is_scoped_to_that_node_only() {
    let function = shapes::add_consts(5, 3);
    let add_node = function
        .walk()
        .find(|&n| {
            matches!(
                function.node_kind(n),
                NodeKind::IntBinaryOp(IntBinaryOp::Add)
            )
        })
        .unwrap();
    let pat = int_const(5u128).into_pattern();
    let result = Matcher::new(&function).match_at(add_node, &pat).unwrap();
    assert!(result.is_none());
}

#[test]
fn find_all_is_deterministic() {
    let function = shapes::add_nested_3(1, 2, 3);
    let matcher = Matcher::new(&function);
    let pat = any_int_const().capture(Capture::new()).into_pattern();

    let a1: Vec<_> = matcher
        .find_all(&pat)
        .unwrap()
        .into_iter()
        .map(|m| m.root())
        .collect();
    let a2: Vec<_> = matcher
        .find_all(&pat)
        .unwrap()
        .into_iter()
        .map(|m| m.root())
        .collect();

    assert_eq!(a1, a2);
}

/// The lazy kind index is built on the first query and reused after; each
/// query at a different discriminant must still return its own hit count.
#[test]
fn kind_index_reused_across_queries() {
    let function = shapes::add_consts(5, 7);
    let matcher = Matcher::new(&function);

    let pat_const = any_int_const().into_pattern();
    assert_eq!(
        matcher.find_all(&pat_const).unwrap().len(),
        2,
        "two IntConsts"
    );

    // Served after the index is built.
    let pat_add = add(any(), any()).into_pattern();
    assert_eq!(matcher.find_all(&pat_add).unwrap().len(), 1, "one Add");

    // Re-run the first query, now via the cache.
    assert_eq!(
        matcher.find_all(&pat_const).unwrap().len(),
        2,
        "re-query still two"
    );
}

/// Three leaf `Add`s at distinct operand values, summed by two more `Add`s.
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
    let hits = Matcher::new(&function).find_all(&pat).unwrap();
    // Three roots x two orderings: `add` is commutative and both captures sit
    // on operands, so each root matches as (lhs,rhs) and as (rhs,lhs).
    assert_eq!(hits.len(), 6);

    let roots: std::collections::HashSet<NodeId> = hits.iter().map(|m| m.root()).collect();
    assert_eq!(
        roots.len(),
        3,
        "the six matches sit on three distinct roots"
    );
}

#[test]
fn each_match_has_its_own_bindings() {
    let function = graph_three_adds();
    let lhs = Capture::new();
    let rhs = Capture::new();
    let pat = add(any_int_const().capture(lhs), any_int_const().capture(rhs)).into_pattern();
    let hits = Matcher::new(&function).find_all(&pat).unwrap();
    // Two operand orderings per root; see
    // `find_all_returns_distinct_matches_with_distinct_roots`.
    assert_eq!(hits.len(), 6);

    let raw: Vec<(u128, u128)> = hits
        .iter()
        .map(|m| {
            (
                m.bindings().get_uint(lhs, &function).unwrap(),
                m.bindings().get_uint(rhs, &function).unwrap(),
            )
        })
        .collect();
    let distinct: std::collections::HashSet<(u128, u128)> = raw.iter().copied().collect();
    assert_eq!(
        distinct.len(),
        6,
        "each match has its own bindings: {raw:?}"
    );

    // Normalised by operand order, the three source `add`s come back, each
    // contributing its pair twice, once per ordering.
    let mut got: Vec<(u128, u128)> = raw
        .into_iter()
        .map(|(l, r)| if l < r { (l, r) } else { (r, l) })
        .collect();
    got.sort();
    assert_eq!(got, vec![(1, 2), (1, 2), (3, 4), (3, 4), (5, 6), (5, 6)]);
}

#[test]
fn bindings_clone_outlives_match() {
    let function = shapes::add_consts(5, 3);
    let v = Capture::new();

    let bindings = {
        let matcher = Matcher::new(&function);
        let node = function
            .walk()
            .find(|&n| {
                matches!(function.node_kind(n), NodeKind::IntConst(_))
                    && function
                        .node_outputs(n)
                        .iter()
                        .any(|&o| function.int_const_u128(o) == Some(5))
            })
            .unwrap();
        let pat = any_int_const().capture(v).into_pattern();
        let m = matcher.match_at(node, &pat).unwrap().expect("match");
        m.bindings_clone()
    };

    assert_eq!(bindings.get_uint(v, &function), Some(5));
}

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
    let m = a::unique(
        &function,
        add(int_const(5u128), int_const(3u128))
            .capture(v)
            .into_pattern(),
    );
    assert_eq!(m.get_vn(v, &function), None);
}

#[test]
fn get_vn_on_unbound_var_returns_none() {
    let function = shapes::add_consts(5, 3);
    let m = a::first(&function, int_const(5u128).into_pattern());
    let never_bound = Capture::new();
    assert_eq!(m.get_vn(never_bound, &function), None);
}

/// With both walk-through flags off, an ordinary query is unaffected.
#[test]
fn existing_pattern_unchanged_with_default_options() {
    let function = shapes::add_consts(5, 3);
    let pat = add(int_const(5u128), int_const(3u128)).into_pattern();
    let hits = Matcher::new(&function).find_all(&pat).unwrap();
    assert_eq!(hits.len(), 1);
}

/// Returns `Add(ZeroExt(Mul(2,3)), 4)` at I64, with the Mul at I32.
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
    let hits = Matcher::new(&function).find_all(&pat).unwrap();
    assert!(hits.is_empty());
}

#[test]
fn add_mul_pattern_matches_through_extend_with_ignore_casts() {
    let function = graph_add_zext_mul();
    // The cast mask lives on the pattern, not the matcher.
    let pat = add(mul(any(), any()), any()).into_pattern().ignore_casts();
    let hits = Matcher::new(&function).find_all(&pat).unwrap();
    assert_eq!(hits.len(), 1);
}

/// A cast walked through via `ignore_casts` is IR the match relied on, so it
/// must appear in `matched_nodes()`. Otherwise a rewrite culling the dead cast
/// loses its address, violating the superset-only fingerprint contract.
#[test]
fn cast_walk_through_records_skipped_cast_in_footprint() {
    let function = graph_add_zext_mul();
    let zext_node = function
        .walk()
        .find(|&n| matches!(function.node_kind(n), NodeKind::Extend(_)))
        .expect("graph has a ZeroExt cast");

    let pat = add(mul(any(), any()), any()).into_pattern().ignore_casts();
    let hits = Matcher::new(&function).find_all(&pat).unwrap();
    assert_eq!(hits.len(), 1);
    assert!(
        hits[0].matched_nodes().contains(&zext_node),
        "the walked-through cast must be in the match footprint, got {:?}",
        hits[0].matched_nodes()
    );
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
    let hits = Matcher::new(&function).find_all(&pat).unwrap();
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
    let m = Matcher::new(&function);
    let pat = truncate(any()).into_pattern().ignore_casts();
    let hits = m.find_all(&pat).unwrap();
    assert_eq!(hits.len(), 1);
    assert!(matches!(
        function.node_kind(hits[0].root()),
        NodeKind::Truncate
    ));
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
    let hits = Matcher::new(&function).find_all(&pat).unwrap();
    assert_eq!(hits.len(), 1);
}

/// Entry region runs `Call`, tail region runs `Return`.
fn graph_ret_via_region_after_call() -> strider_ir::Function {
    let mut t = Tb::bare(vec![], &[], &[], &[], None, 0);
    let head = t.fb_mut().create_region_all().expect("head");
    t.fb_mut().set_entry_region_all(head).expect("entry head");
    t.fb_mut().set_region(head);

    let target = t
        .fb_mut()
        .build_int_const(0xCAFEu64, ValueType::I64)
        .unwrap();
    t.fb_mut().build_call_cc(target, None).expect("call");

    let tail = t.fb_mut().create_region_all().expect("tail");
    t.fb_mut().build_branch(tail).expect("branch to tail");

    t.fb_mut().set_region(tail);
    t.fb_mut().build_return(None, &[]).expect("ret");

    t.finish()
}

#[test]
fn ret_call_does_not_match_through_region_by_default() {
    let function = graph_ret_via_region_after_call();
    // "Return preceded by a Call" goes through a node-level filter: there is no
    // Call-kind value wildcard, and `preceded_by` inspects only the direct ctrl
    // predecessor, which here is the tail Region rather than the Call.
    let pat = ret()
        .preceded_by(any().filter(|m, node| {
            matches!(
                m.function().node_kind(node),
                strider_ir::node::NodeKind::Call
            )
        }))
        .build();
    let hits = Matcher::new(&function).find_all(&pat).unwrap();
    assert!(hits.is_empty());
}

#[test]
fn matches_next_concrete_kind_equals_find_all_first() {
    let function = shapes::add_nested_3(2, 3, 5);
    let m = Matcher::new(&function);
    let p = add(any(), any()).into_pattern();
    let all = m.find_all(&p).unwrap();
    assert!(!all.is_empty(), "fixture has Add nodes");
    let first = m
        .matches(&p)
        .unwrap()
        .next()
        .expect("at least one Add matches");
    assert_eq!(first.root(), all[0].root());
}

#[test]
fn matches_next_wildcard_equals_find_all_first() {
    let function = shapes::add_consts(1, 2);
    let m = Matcher::new(&function);
    let p = any().into_pattern();
    let all = m.find_all(&p).unwrap();
    assert!(!all.is_empty());
    let first = m
        .matches(&p)
        .unwrap()
        .next()
        .expect("wildcard matches some node");
    assert_eq!(first.root(), all[0].root());
}

#[test]
fn matches_next_no_match_returns_none() {
    let function = shapes::add_consts(5, 3);
    let m = Matcher::new(&function);
    assert!(m.matches(&load().build()).unwrap().next().is_none());
    assert!(m.matches(&call().build()).unwrap().next().is_none());
}

#[test]
fn find_joined_empty_input() {
    let function = shapes::add_consts(2, 3);
    let m = Matcher::new(&function);
    let results = m.find_joined_constrained(&[], &[]).unwrap();
    assert!(results.is_empty());
}

#[test]
fn find_joined_single_pattern_equivalent_to_find_all() {
    let function = shapes::add_consts(2, 3);
    let m = Matcher::new(&function);
    let p = add(any(), any()).into_pattern();
    let req = m.find_joined_constrained(&[&p], &[]).unwrap();
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
    let m = Matcher::new(&function);
    let p_add = add(any(), any()).into_pattern();
    let p_call = call().build();
    let req = m.find_joined_constrained(&[&p_add, &p_call], &[]).unwrap();
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

    let mr = Matcher::new(&function);
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

    let req = mr.find_joined_constrained(&[&p_zero, &p_99], &[]).unwrap();
    assert_eq!(req.len(), 1);
    let inner = &req[0];
    assert_eq!(inner.len(), 2);

    let s1 = inner[0]
        .node(shared, function.graph())
        .expect("shared bound in pat[0]");
    let s2 = inner[1]
        .node(shared, function.graph())
        .expect("shared bound in pat[1]");
    assert_eq!(s1, s2);

    let k_val = inner[0].bindings().get_uint(k, &function).expect("K bound");
    assert_eq!(k_val, 8);
}

/// A joined tuple exists only when all three patterns bind the shared capture
/// to the same node.
#[test]
fn find_joined_three_patterns_all_agree_on_shared_capture() {
    let mut t = Tb::empty();
    let base = t.u64(0xAAAA);
    let off8 = t.u64(8);
    let off16 = t.u64(16);
    let off24 = t.u64(24);
    let zero = t.u64(0);
    let v99 = t.u64(99);
    let v7 = t.u64(7);
    let a1 = t.add(base, off8);
    let a2 = t.add(base, off16);
    let a3 = t.add(base, off24);
    t.store_ram(a1, zero);
    t.store_ram(a2, v99);
    t.store_ram(a3, v7);
    let function = t.ret_nothing();

    let mr = Matcher::new(&function);
    let shared = Capture::new();
    let store_pat = |off: u128, data: u128| {
        store()
            .addr(add(var(shared), int_const(off)).ordered())
            .data(int_const(data))
            .build()
    };
    let p0 = store_pat(8, 0);
    let p1 = store_pat(16, 99);
    let p2 = store_pat(24, 7);

    let req = mr.find_joined_constrained(&[&p0, &p1, &p2], &[]).unwrap();
    assert_eq!(req.len(), 1, "one fully-agreeing triple");
    let inner = &req[0];
    assert_eq!(inner.len(), 3, "one Match per pattern");
    let s0 = inner[0].node(shared, function.graph()).unwrap();
    let s1 = inner[1].node(shared, function.graph()).unwrap();
    let s2 = inner[2].node(shared, function.graph()).unwrap();
    assert_eq!(s0, s1);
    assert_eq!(s1, s2);
}

/// Same three-pattern join, but the third store hangs off a different base, so
/// its `shared` binding disagrees and no triple survives.
#[test]
fn find_joined_three_patterns_one_disagrees_yields_empty() {
    let mut t = Tb::empty();
    let base = t.u64(0xAAAA);
    let other = t.u64(0xBBBB);
    let off8 = t.u64(8);
    let off16 = t.u64(16);
    let off24 = t.u64(24);
    let zero = t.u64(0);
    let v99 = t.u64(99);
    let v7 = t.u64(7);
    let a1 = t.add(base, off8);
    let a2 = t.add(base, off16);
    let a3 = t.add(other, off24); // disagreeing base
    t.store_ram(a1, zero);
    t.store_ram(a2, v99);
    t.store_ram(a3, v7);
    let function = t.ret_nothing();

    let mr = Matcher::new(&function);
    let shared = Capture::new();
    let store_pat = |off: u128, data: u128| {
        store()
            .addr(add(var(shared), int_const(off)).ordered())
            .data(int_const(data))
            .build()
    };
    let p0 = store_pat(8, 0);
    let p1 = store_pat(16, 99);
    let p2 = store_pat(24, 7);

    let req = mr.find_joined_constrained(&[&p0, &p1, &p2], &[]).unwrap();
    assert!(req.is_empty(), "third pattern's shared binding disagrees");
}

/// Connectivity is ORDER-INDEPENDENT: a pattern bridging to the rest only via a
/// pattern listed *after* it must not be rejected. The old prefix-only check
/// rejected e.g. `guard(var(fop))` + `call(var(fv))` +
/// `load(...).capture(fop, fv)` whenever the bridge wasn't listed first.
#[test]
fn find_joined_connectivity_is_order_independent() {
    let mut t = Tb::empty();
    let base = t.u64(0xAAAA);
    let off8 = t.u64(8);
    let off16 = t.u64(16);
    let data = t.u64(0xD);
    let a1 = t.add(base, off8);
    let a2 = t.add(base, off16);
    t.store_ram(a1, data);
    t.store_ram(a2, data);
    let function = t.ret_nothing();
    let mr = Matcher::new(&function);

    let x = Capture::new(); // shared base
    let d = Capture::new(); // shared data
    // `bridge` binds both x and d; `by_base` binds only x, `by_data` only d, so
    // those two share nothing directly and connect ONLY through `bridge`.
    let bridge = store()
        .addr(add(var(x), int_const(8u128)).ordered())
        .data(var(d))
        .build();
    let by_base = store()
        .addr(add(var(x), int_const(16u128)).ordered())
        .build();
    let by_data = store().data(var(d)).build();

    for order in [
        [&by_base, &by_data, &bridge],
        [&by_data, &by_base, &bridge],
        [&bridge, &by_base, &by_data],
    ] {
        assert!(
            mr.find_joined_constrained(&order, &[]).is_ok(),
            "connected patterns must join regardless of order",
        );
    }

    // A genuinely disconnected capture group still errors.
    let z = Capture::new();
    let disjoint = store().data(var(z)).build();
    assert!(
        mr.find_joined_constrained(&[&bridge, &disjoint], &[])
            .is_err(),
        "patterns sharing no capture even transitively are still rejected",
    );
}

/// A pattern that does NOT mention the shared capture imposes no constraint:
/// the join degrades to a cross product against its matches.
#[test]
fn find_joined_pattern_without_shared_capture_cross_products() {
    let mut t = Tb::empty();
    let base_a = t.u64(0xAAAA);
    let base_b = t.u64(0xBBBB);
    let off8 = t.u64(8);
    let off16 = t.u64(16);
    let zero = t.u64(0);
    let a1 = t.add(base_a, off8);
    let a2 = t.add(base_b, off16);
    t.store_ram(a1, zero);
    t.store_ram(a2, zero);
    t.call_at(0x1234);
    let function = t.ret_nothing();

    let mr = Matcher::new(&function);
    let shared = Capture::new();
    // The store binds `shared` twice over different bases; the Call never
    // mentions it.
    let p_store = store()
        .addr(add(var(shared), any_int_const().capture(Capture::new())).ordered())
        .data(int_const(0u128))
        .build();
    let p_call = call().at(0x1234).build();

    let req = mr
        .find_joined_constrained(&[&p_store, &p_call], &[])
        .unwrap();
    assert_eq!(req.len(), 2, "no shared capture in B: pure cross product");
    for tuple in &req {
        assert_eq!(tuple.len(), 2);
        assert!(tuple[0].node(shared, function.graph()).is_some());
        assert!(tuple[1].node(shared, function.graph()).is_none());
    }
    let s0 = req[0][0].node(shared, function.graph()).unwrap();
    let s1 = req[1][0].node(shared, function.graph()).unwrap();
    assert_ne!(s0, s1);
}

/// A zero-match pattern in FIRST position collapses the whole join to an empty
/// top-level Vec (no per-slot empties), like the second-position case.
#[test]
fn find_joined_zero_match_first_pattern_yields_empty() {
    let function = shapes::add_consts(2, 3);
    let m = Matcher::new(&function);
    let p_call = call().build(); // no Call in the graph
    let p_add = add(any(), any()).into_pattern();
    let req = m.find_joined_constrained(&[&p_call, &p_add], &[]).unwrap();
    assert!(req.is_empty());
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

    let mr = Matcher::new(&function);
    let shared = Capture::new();
    let p_8 = store()
        .addr(add(var(shared), int_const(8u128)).ordered())
        .data(int_const(0u128))
        .build();
    let p_16 = store()
        .addr(add(var(shared), int_const(16u128)).ordered())
        .data(int_const(0u128))
        .build();
    let req = mr.find_joined_constrained(&[&p_8, &p_16], &[]).unwrap();
    assert!(req.is_empty());
}

/// Two patterns that EACH declare captures but share NONE of them is a caller
/// bug (a mis-wired correlation), not a request for a cartesian product, so it
/// must error rather than return |adds|^2 meaningless tuples. A capture-FREE
/// pattern stays exempt; see
/// `find_joined_pattern_without_shared_capture_cross_products`.
#[test]
fn find_joined_disjoint_captures_is_rejected() {
    let function = shapes::add_nested_3(1, 2, 3);
    let mr = Matcher::new(&function);
    let a0 = Capture::new();
    let a1 = Capture::new();
    let b0 = Capture::new();
    let b1 = Capture::new();
    let p_a = add(any().capture(a0), any().capture(a1)).into_pattern();
    let p_b = add(any().capture(b0), any().capture(b1)).into_pattern();
    let Err(err) = mr.find_joined_constrained(&[&p_a, &p_b], &[]) else {
        panic!("disjoint-capture join must error, not return tuples");
    };
    let msg = err.to_string();
    assert!(
        msg.contains("shares no capture"),
        "error should name the no-shared-capture join bug, got: {msg}"
    );
}

/// Tuples that agree on every shared capture but differ only in WHICH
/// correlated site they pair must dedup, or a consumer acting per shared
/// binding double-acts. Here two stores share a base, so the raw cross-product
/// is 2*2 = 4 tuples that must collapse to one.
#[test]
fn find_joined_dedups_tuples_equivalent_on_shared_captures() {
    let mut t = Tb::empty();
    let base = t.u64(0xAAAA);
    let off8 = t.u64(8);
    let off16 = t.u64(16);
    let zero = t.u64(0);
    let a1 = t.add(base, off8);
    let a2 = t.add(base, off16);
    t.store_ram(a1, zero);
    t.store_ram(a2, zero);
    let function = t.ret_nothing();

    let mr = Matcher::new(&function);
    let shared = Capture::new();
    // The offset operand is an uncaptured wildcard, so both stores bind
    // identical shared sets.
    let p0 = store()
        .addr(add(var(shared), any()).ordered())
        .data(int_const(0u128))
        .build();
    let p1 = store()
        .addr(add(var(shared), any()).ordered())
        .data(int_const(0u128))
        .build();

    let req = mr.find_joined_constrained(&[&p0, &p1], &[]).unwrap();
    assert_eq!(
        req.len(),
        1,
        "tuples equivalent on the shared capture must dedup to one"
    );
}

/// `any()` matches every reachable node of a minimal function, value-less
/// kinds (Entry, Region, MemPhi, InitialMemory) included.
#[test]
fn find_all_any_on_minimal_function_pins_node_count() {
    let function = Tb::empty().ret_nothing();
    let m = Matcher::new(&function);
    let hits = m.find_all(&any().into_pattern()).unwrap();
    let walked = function.walk().count();
    assert_eq!(hits.len(), walked, "any() matches every reachable node");
    assert_eq!(
        hits.len(),
        5,
        "Entry + Region + MemPhi + InitialMemory + Return"
    );
}

/// The value-less control root matches `ret()` and the bare wildcard, but not
/// a value-typed pattern.
#[test]
fn match_at_on_return_node_of_minimal_function() {
    let function = Tb::empty().ret_nothing();
    let ret_node = a::find_node(&function, |k| matches!(k, NodeKind::Return));
    let m = Matcher::new(&function);

    let hit = m
        .match_at(ret_node, &ret().build())
        .unwrap()
        .expect("ret() matches Return");
    assert_eq!(hit.root(), ret_node);

    let wild = m.match_at(ret_node, &any().into_pattern()).unwrap();
    assert!(
        wild.is_some(),
        "any() matches the value-less Return at match_at"
    );

    assert!(
        m.match_at(ret_node, &any_int_const().into_pattern())
            .unwrap()
            .is_none()
    );
}
