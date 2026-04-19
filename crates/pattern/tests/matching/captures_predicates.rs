use ir::{
    FunctionBuilder, IntBinaryOp,
    node::{NodeKind, NodeOutputType},
};
use pattern::*;

use super::common::*;

// ── Var uniqueness ────────────────────────────────────────────────────────────

#[test]
fn var_ids_are_unique() -> ir::Result<()> {
    let a = Var::new();
    let b = Var::new();
    let c = Var::new();
    assert_ne!(a, b);
    assert_ne!(b, c);
    assert_ne!(a, c);
    Ok(())
}

#[test]
fn node_var_ids_are_unique_and_distinct_from_var() -> ir::Result<()> {
    let v = Var::new();
    let nv = NodeVar::new();
    // Their raw ids come from the same counter so they can't collide.
    // We can only check that two NodeVars differ from each other.
    let nv2 = NodeVar::new();
    assert_ne!(nv, nv2);
    let _ = v; // used
    Ok(())
}

// ── Capture variables ─────────────────────────────────────────────────────────

#[test]
fn capture_var_binds_to_matched_output() -> ir::Result<()> {
    let g = graph_and_add_return()?;
    let m = Matcher::new(&g);
    let x = Var::new();
    // add(and(4, x), 1) — x should bind to the const-7 output
    let hits = m.find_all(&add(and(int_const(4), var(x)), int_const(1)).into());
    assert_eq!(hits.len(), 1);

    let bound = hits[0].get(x).expect("x should be bound");
    // The bound output should come from an IntConst(7) node.
    let node = g.graph.get_node_from_output(bound);
    assert!(
        matches!(g.graph.node_kind(node), NodeKind::IntConst(7)),
        "x should be bound to const 7, got {:?}",
        g.graph.node_kind(node)
    );
    Ok(())
}

#[test]
fn same_var_twice_requires_same_output() -> ir::Result<()> {
    // add(x, x) should only match if both inputs are identical.
    // In graph_add_return, add(5, 3) — both inputs are different constants.
    let g = graph_add_return()?;
    let m = Matcher::new(&g);
    let x = Var::new();
    let hits = m.find_all(&add(var(x), var(x)).into());
    assert!(hits.is_empty(), "add(x,x) must not match add(5,3)");
    Ok(())
}

#[test]
fn same_var_twice_matches_when_operands_are_equal() -> ir::Result<()> {
    // Build a graph with add(c, c) where both inputs are the same node.
    // Because constants are deduplicated, build_int_const(5,U64) twice
    // returns the same NodeOutputId.
    let mut b = FunctionBuilder::new(vec![], &[], &[], &[])?;
    let r = b.create_region()?;
    b.set_entry_region(r)?;
    b.set_region(r);
    let c = b.build_int_const(5, NodeOutputType::U64);
    let sum = b.build_int_binary_operation(c, c, IntBinaryOp::Add, NodeOutputType::U64)?;
    b.build_return(Some(sum), &[])?;
    let g = b.build().expect("build failed: validator rejected graph");

    let m = Matcher::new(&g);
    let x = Var::new();
    let hits = m.find_all(&add(var(x), var(x)).into());
    assert_eq!(hits.len(), 1, "add(x,x) must match add(c,c) with same node");
    // Both uses of x resolve to the same constant output.
    let bound = hits[0].get(x).unwrap();
    let node = g.graph.get_node_from_output(bound);
    assert!(matches!(g.graph.node_kind(node), NodeKind::IntConst(5)));
    Ok(())
}

#[test]
fn two_independent_vars_bind_independently() -> ir::Result<()> {
    let g = graph_and_add_return()?;
    let m = Matcher::new(&g);
    let lhs_var = Var::new();
    let rhs_var = Var::new();
    // add(and(lhs_var, rhs_var), 1)
    let hits = m.find_all(&add(and(var(lhs_var), var(rhs_var)), int_const(1)).into());
    assert_eq!(hits.len(), 1);
    let lhs_bound = hits[0].get(lhs_var).expect("lhs_var must be bound");
    let rhs_bound = hits[0].get(rhs_var).expect("rhs_var must be bound");
    // They should be different nodes (4 vs 7)
    assert_ne!(lhs_bound, rhs_bound);
    let lhs_node = g.graph.get_node_from_output(lhs_bound);
    let rhs_node = g.graph.get_node_from_output(rhs_bound);
    assert!(matches!(g.graph.node_kind(lhs_node), NodeKind::IntConst(4)));
    assert!(matches!(g.graph.node_kind(rhs_node), NodeKind::IntConst(7)));
    Ok(())
}

#[test]
fn var_shared_across_nested_subpatterns_enforces_equality() -> ir::Result<()> {
    // add(x, x) where x appears twice — must match only if both inputs equal.
    // add(1, 2) has different inputs so should NOT match.
    let g = graph_nested_add()?;
    let m = Matcher::new(&g);
    let x = Var::new();
    // add(x, x) — both leaves must be the same node
    let hits = m.find_all(&add(var(x), var(x)).into());
    assert!(
        hits.is_empty(),
        "add(x,x) must not match add(1,2) or add(add(1,2),3)"
    );
    Ok(())
}

// ── NodeVar capture ───────────────────────────────────────────────────────────

#[test]
fn node_var_captures_call_node_id() -> ir::Result<()> {
    let g = graph_call_return()?;
    let m = Matcher::new(&g);
    let cv = NodeVar::new();
    let hits = m.find_all(&call().at(0x1234).capture(cv).into());
    assert_eq!(hits.len(), 1);
    let node_id = hits[0].get_node(cv).expect("NodeVar must be bound");
    assert!(matches!(g.graph.node_kind(node_id), NodeKind::Call));
    Ok(())
}

#[test]
fn ret_node_var_captures_return_node_id() -> ir::Result<()> {
    let g = graph_add_return()?;
    let m = Matcher::new(&g);
    let rv = NodeVar::new();
    let hits = m.find_all(&ret().capture(rv).into());
    assert_eq!(hits.len(), 1);
    let node_id = hits[0].get_node(rv).expect("NodeVar must be bound");
    assert_eq!(node_id, hits[0].root);
    assert!(matches!(g.graph.node_kind(node_id), NodeKind::Return));
    Ok(())
}

#[test]
fn if_node_var_captures_if_node_id() -> ir::Result<()> {
    let g = graph_if_branches()?;
    let m = Matcher::new(&g);
    let iv = NodeVar::new();
    let hits = m.find_all(&if_node().capture(iv).into());
    assert_eq!(hits.len(), 1);
    let node_id = hits[0].get_node(iv).unwrap();
    assert!(matches!(g.graph.node_kind(node_id), NodeKind::If));
    Ok(())
}

#[test]
fn node_var_not_bound_when_pattern_fails() -> ir::Result<()> {
    let g = graph_add_return()?;
    let m = Matcher::new(&g);
    let cv = NodeVar::new();
    // call().at(0xDEAD) won't match, so cv stays unbound
    let hits = m.find_all(&call().at(0xDEAD).capture(cv).into());
    assert!(hits.is_empty());
    Ok(())
}

// ── any() wildcard ────────────────────────────────────────────────────────────

#[test]
fn any_pattern_matches_many_nodes() -> ir::Result<()> {
    // any() as a data root matches all nodes that have a value output.
    // At minimum the constants and the add node.
    let g = graph_add_return()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&any());
    // 5 const, 3 const, add result all have value outputs → ≥ 3 hits
    assert!(
        hits.len() >= 3,
        "expected at least 3 any() matches, got {}",
        hits.len()
    );
    Ok(())
}

#[test]
fn any_in_binary_op_matches_both_operands() -> ir::Result<()> {
    // add(any(), any()) matches any add node regardless of operands.
    let g = graph_add_return()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&add(any(), any()).into());
    assert_eq!(hits.len(), 1);
    Ok(())
}

// ── No spurious matches ───────────────────────────────────────────────────────

#[test]
fn call_pattern_no_match_in_add_only_graph() -> ir::Result<()> {
    let g = graph_add_return()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&call().into());
    assert!(hits.is_empty());
    Ok(())
}

#[test]
fn if_pattern_no_match_in_call_graph() -> ir::Result<()> {
    let g = graph_call_return()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&if_node().into());
    assert!(hits.is_empty());
    Ok(())
}

#[test]
fn mul_pattern_no_match_in_add_graph() -> ir::Result<()> {
    let g = graph_add_return()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&mul(any(), any()).into());
    assert!(hits.is_empty());
    Ok(())
}

#[test]
fn zero_extend_no_match_in_add_graph() -> ir::Result<()> {
    let g = graph_add_return()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&zero_extend(any()));
    assert!(hits.is_empty());
    Ok(())
}

// ── commutative matching ──────────────────────────────────────────────────────

#[test]
fn commutative_add_reversed_operands_matches() -> ir::Result<()> {
    // IR has add(5, 3); pattern asks for add(3, 5) — should match via commutation.
    let g = graph_add_5_3()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&add(int_const(3), int_const(5)).into());
    assert_eq!(
        hits.len(),
        1,
        "commutative add should match reversed operands"
    );
    Ok(())
}

#[test]
fn commutative_add_stated_order_also_matches() -> ir::Result<()> {
    // Stated order (5, 3) should still work.
    let g = graph_add_5_3()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&add(int_const(5), int_const(3)).into());
    assert_eq!(hits.len(), 1);
    Ok(())
}

#[test]
fn ordered_add_reversed_operands_no_match() -> ir::Result<()> {
    // .ordered() forces stated order — reversed should not match.
    let g = graph_add_5_3()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&add(int_const(3), int_const(5)).ordered().into());
    assert!(
        hits.is_empty(),
        "ordered add must not match reversed operands"
    );
    Ok(())
}

#[test]
fn non_commutative_sub_no_commutation() -> ir::Result<()> {
    // sub(5, 3) — pattern sub(3, 5) must NOT match even without .ordered().
    let g = graph_sub_5_3()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&sub(int_const(3), int_const(5)).into());
    assert!(
        hits.is_empty(),
        "sub is not commutative and must not match reversed operands"
    );
    Ok(())
}

#[test]
fn commutative_add_with_wildcard_lhs() -> ir::Result<()> {
    // add(_, 5) should match add(5, 3) via commutation (try: l=5 vs _, r=3 vs 5 fails;
    // then reversed: l=3 vs _, r=5 vs 5 succeeds).
    let g = graph_add_5_3()?;
    let m = Matcher::new(&g);
    // Pattern: add(_, int_const(3)) — rhs is 3.
    let hits = m.find_all(&add(any(), int_const(3)).into());
    assert_eq!(hits.len(), 1);
    Ok(())
}

// ── .capture() on any pattern ─────────────────────────────────────────────────

#[test]
fn capture_output_of_add_node() -> ir::Result<()> {
    // Capture the output of the add node itself.
    let g = graph_add_5_3()?;
    let m = Matcher::new(&g);
    let v = Var::new();
    let hits = m.find_all(&add(any(), any()).capture(v));
    assert_eq!(hits.len(), 1);
    // The captured output should be the add node's output — its kind is IntBinaryOp.
    let out = hits[0].get(v).expect("capture var should be bound");
    let node = g.graph.get_node_from_output(out);
    assert!(matches!(g.graph.node_kind(node), NodeKind::IntBinaryOp(_)));
    Ok(())
}

#[test]
fn capture_nested_field_via_var() -> ir::Result<()> {
    // Capture the address of a load by passing var(v) as the addr sub-pattern.
    let mut b = FunctionBuilder::new(vec![], &[], &[], &[])?;
    let r = b.create_region()?;
    b.set_entry_region(r)?;
    b.set_region(r);
    let addr_const = b.build_int_const(0x1000, NodeOutputType::U64);
    let loaded = b.build_load(
        addr_const,
        rsleigh::VnSpace::RAM,
        ir::node::NodeOutputType::U64,
    )?;
    b.build_return(Some(loaded), &[])?;
    let g = b.build().expect("build failed: validator rejected graph");

    let m = Matcher::new(&g);
    let addr_v = Var::new();
    let hits = m.find_all(&load().addr(var(addr_v)).into());
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].get_int_const(addr_v, &g), Some(0x1000));
    Ok(())
}

#[test]
fn capture_via_when_on_load_addr() -> ir::Result<()> {
    // Use any().capture(v) as sub-pattern to capture the load's address output.
    let mut b = FunctionBuilder::new(vec![], &[], &[], &[])?;
    let r = b.create_region()?;
    b.set_entry_region(r)?;
    b.set_region(r);
    let addr_const = b.build_int_const(0x2000, NodeOutputType::U64);
    let loaded = b.build_load(
        addr_const,
        rsleigh::VnSpace::RAM,
        ir::node::NodeOutputType::U64,
    )?;
    b.build_return(Some(loaded), &[])?;
    let g = b.build().expect("build failed: validator rejected graph");

    let m = Matcher::new(&g);
    let addr_v = Var::new();
    // any().capture(v) is equivalent to var(v).
    let hits = m.find_all(&load().addr(any().capture(addr_v)).into());
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].get_int_const(addr_v, &g), Some(0x2000));
    Ok(())
}

// ── .when() predicate ─────────────────────────────────────────────────────────

#[test]
fn when_predicate_filters_matching_nodes() -> ir::Result<()> {
    // any().when(f) should only match outputs where f returns true.
    let g = graph_add_5_3()?;
    let m = Matcher::new(&g);
    // Predicate: only match IntConst nodes.
    let hits = m.find_all(&any().when(|fg, _ty, out| {
        let node = fg.graph.get_node_from_output(out);
        matches!(fg.graph.node_kind(node), NodeKind::IntConst(_))
    }));
    // There are two IntConst nodes (5 and 3).
    assert_eq!(hits.len(), 2);
    Ok(())
}

#[test]
fn predicate_fn_matches_same_as_when() -> ir::Result<()> {
    // predicate(f) is equivalent to any().when(f).
    let g = graph_add_5_3()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&predicate(|fg, _ty, out| {
        let node = fg.graph.get_node_from_output(out);
        matches!(fg.graph.node_kind(node), NodeKind::IntConst(_))
    }));
    assert_eq!(hits.len(), 2);
    Ok(())
}

#[test]
fn when_predicate_on_structural_pattern() -> ir::Result<()> {
    // add(_, _).when(f) — structural match first, then predicate.
    let g = graph_add_5_3()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&add(any(), any()).when(|_fg, ty, _out| {
        // Only match if the add node's output is a U64.
        ty == ir::node::NodeOutputType::U64
    }));
    assert_eq!(hits.len(), 1);
    Ok(())
}

#[test]
fn when_predicate_rejection() -> ir::Result<()> {
    // .when(f) that always returns false rejects everything.
    let g = graph_add_5_3()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&any().when(|_fg, _ty, _out| false));
    assert!(hits.is_empty());
    Ok(())
}

#[test]
fn predicate_in_field_position() -> ir::Result<()> {
    // predicate(f) passed as load's addr sub-pattern.
    let mut b = FunctionBuilder::new(vec![], &[], &[], &[])?;
    let r = b.create_region()?;
    b.set_entry_region(r)?;
    b.set_region(r);
    let addr_const = b.build_int_const(0x3000, NodeOutputType::U64);
    let loaded = b.build_load(
        addr_const,
        rsleigh::VnSpace::RAM,
        ir::node::NodeOutputType::U64,
    )?;
    b.build_return(Some(loaded), &[])?;
    let g = b.build().expect("build failed: validator rejected graph");

    let m = Matcher::new(&g);
    // Match loads where the address is an IntConst >= 0x1000.
    let hits = m.find_all(
        &load()
            .addr(predicate(|fg, _ty, out| {
                let node = fg.graph.get_node_from_output(out);
                match fg.graph.node_kind(node) {
                    NodeKind::IntConst(v) => *v >= 0x1000,
                    _ => false,
                }
            }))
            .into(),
    );
    assert_eq!(hits.len(), 1);
    Ok(())
}
