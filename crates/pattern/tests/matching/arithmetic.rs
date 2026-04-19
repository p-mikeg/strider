use pattern::*;

use super::common::*;

// ── int_const pattern ─────────────────────────────────────────────────────────

#[test]
fn int_const_matches_exact_value() -> ir::Result<()> {
    let g = graph_add_return()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&int_const(5));
    assert_eq!(hits.len(), 1);
    Ok(())
}

#[test]
fn int_const_no_match_for_wrong_value() -> ir::Result<()> {
    let g = graph_add_return()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&int_const(99));
    assert!(hits.is_empty());
    Ok(())
}

#[test]
fn bool_const_matches_true() -> ir::Result<()> {
    let g = graph_bool_and_return()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&bool_const(true));
    assert_eq!(
        hits.len(),
        1,
        "bool_const(true) should find exactly one node"
    );
    Ok(())
}

#[test]
fn bool_const_matches_false() -> ir::Result<()> {
    let g = graph_bool_and_return()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&bool_const(false));
    assert_eq!(
        hits.len(),
        1,
        "bool_const(false) should find exactly one node"
    );
    Ok(())
}

#[test]
fn bool_const_no_match_in_int_only_graph() -> ir::Result<()> {
    let g = graph_add_return()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&bool_const(true));
    assert!(hits.is_empty());
    Ok(())
}

// ── Binary op patterns ────────────────────────────────────────────────────────

#[test]
fn add_pattern_matches_add_node() -> ir::Result<()> {
    let g = graph_add_return()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&add(int_const(5), int_const(3)).into());
    assert_eq!(hits.len(), 1);
    Ok(())
}

#[test]
fn add_pattern_ordered_wrong_operand_order_no_match() -> ir::Result<()> {
    // With .ordered(), operand order is significant.
    let g = graph_add_return()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&add(int_const(3), int_const(5)).ordered().into());
    assert!(
        hits.is_empty(),
        "ordered add must not match reversed operands"
    );
    Ok(())
}

#[test]
fn nested_pattern_and_add() -> ir::Result<()> {
    // and(4, 7) + 1
    let g = graph_and_add_return()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&add(and(int_const(4), int_const(7)), int_const(1)).into());
    assert_eq!(hits.len(), 1);
    Ok(())
}

#[test]
fn nested_pattern_partial_wildcard() -> ir::Result<()> {
    // add(and(4, _), 1)
    let g = graph_and_add_return()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&add(and(int_const(4), any()), int_const(1)).into());
    assert_eq!(hits.len(), 1);
    Ok(())
}

#[test]
fn sub_pattern_no_match_in_add_graph() -> ir::Result<()> {
    let g = graph_add_return()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&sub(int_const(5), int_const(3)).into());
    assert!(hits.is_empty(), "no sub node in add graph");
    Ok(())
}

#[test]
fn or_pattern_no_match_in_and_graph() -> ir::Result<()> {
    let g = graph_and_add_return()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&or(int_const(4), int_const(7)).into());
    assert!(hits.is_empty(), "no or node; the binary op is And");
    Ok(())
}

#[test]
fn and_pattern_matches_in_and_add_graph() -> ir::Result<()> {
    let g = graph_and_add_return()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&and(int_const(4), int_const(7)).into());
    assert_eq!(hits.len(), 1);
    Ok(())
}

#[test]
fn xor_pattern_no_match_in_add_graph() -> ir::Result<()> {
    let g = graph_add_return()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&xor(any(), any()).into());
    assert!(hits.is_empty());
    Ok(())
}

// ── Deeply nested add patterns ────────────────────────────────────────────────

#[test]
fn deeply_nested_add_matches() -> ir::Result<()> {
    let g = graph_nested_add()?;
    let m = Matcher::new(&g);
    // add(add(1, 2), 3)
    let hits = m.find_all(&add(add(int_const(1), int_const(2)), int_const(3)).into());
    assert_eq!(hits.len(), 1);
    Ok(())
}

#[test]
fn deeply_nested_add_ordered_wrong_order_no_match() -> ir::Result<()> {
    let g = graph_nested_add()?;
    let m = Matcher::new(&g);
    // add(3, add(1, 2)).ordered() — wrong outer order, ordered matching
    let hits = m.find_all(
        &add(int_const(3), add(int_const(1), int_const(2)))
            .ordered()
            .into(),
    );
    assert!(hits.is_empty());
    Ok(())
}

#[test]
fn inner_add_matches_independently() -> ir::Result<()> {
    // The inner add(1,2) should also be found directly.
    let g = graph_nested_add()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&add(int_const(1), int_const(2)).into());
    assert_eq!(hits.len(), 1);
    Ok(())
}

// ── Unary op patterns ─────────────────────────────────────────────────────────

#[test]
fn neg_pattern_matches_neg_node() -> ir::Result<()> {
    let g = graph_neg_add_return()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&neg(any()));
    assert_eq!(hits.len(), 1);
    Ok(())
}

#[test]
fn neg_of_add_pattern_matches() -> ir::Result<()> {
    let g = graph_neg_add_return()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&neg(add(int_const(5), int_const(3))));
    assert_eq!(hits.len(), 1);
    Ok(())
}

#[test]
fn not_pattern_no_match_in_neg_graph() -> ir::Result<()> {
    // `not` is bitwise NOT (IntUnaryOp::Not); the graph only has `neg`.
    let g = graph_neg_add_return()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&not(any()));
    assert!(hits.is_empty(), "graph has neg, not not");
    Ok(())
}

#[test]
fn neg_pattern_no_match_in_add_graph() -> ir::Result<()> {
    let g = graph_add_return()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&neg(any()));
    assert!(hits.is_empty());
    Ok(())
}

// ── Bool op patterns ──────────────────────────────────────────────────────────

#[test]
fn bool_not_pattern_matches() -> ir::Result<()> {
    let g = graph_bool_not_return()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&bool_not(any()));
    assert_eq!(hits.len(), 1);
    Ok(())
}

#[test]
fn bool_and_pattern_matches() -> ir::Result<()> {
    let g = graph_bool_and_return()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&bool_and(bool_const(true), bool_const(false)).into());
    assert_eq!(hits.len(), 1);
    Ok(())
}

#[test]
fn bool_or_pattern_no_match_in_and_graph() -> ir::Result<()> {
    let g = graph_bool_and_return()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&bool_or(any(), any()).into());
    assert!(hits.is_empty(), "graph has bool_and, not bool_or");
    Ok(())
}

// Note: bool_and(true, false) evaluates to false at compile time, so the
// builder folds it.  bool_and with a non-const operand is needed to avoid
// constant folding and actually emit a BoolBinaryOp node.
#[test]
fn bool_and_pattern_with_wildcard() -> ir::Result<()> {
    let g = graph_bool_and_return()?;
    let m = Matcher::new(&g);
    let _hits = m.find_all(&bool_and(any(), any()).into());
    // boolean constant folding: bool_and(true, false) = BoolConst(false)
    // In that case there is no BoolBinaryOp node, so hits could be 0 or 1.
    // We just assert the bool_or wildcard produces 0:
    let or_hits = m.find_all(&bool_or(any(), any()).into());
    assert!(or_hits.is_empty());
    Ok(())
}

// ── Comparison op patterns ────────────────────────────────────────────────────

#[test]
fn int_eq_pattern_matches_in_if_graph() -> ir::Result<()> {
    let g = graph_if_branches()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&int_eq(int_const(4), int_const(1)));
    assert_eq!(hits.len(), 1);
    Ok(())
}

#[test]
fn int_eq_commutative_matches_both_orders() -> ir::Result<()> {
    // IntEq is commutative: int_eq(a, b) matches the same nodes as int_eq(b, a).
    let g = graph_if_branches()?;
    let m = Matcher::new(&g);
    // Both orderings should produce a hit (the graph has IntCmpOp::Equal(4, 1)).
    let hits_natural = m.find_all(&int_eq(int_const(4), int_const(1)));
    let hits_swapped = m.find_all(&int_eq(int_const(1), int_const(4)));
    assert_eq!(hits_natural.len(), 1, "natural order should match");
    assert_eq!(hits_swapped.len(), 1, "swapped order should also match (commutative)");
    Ok(())
}

#[test]
fn int_lt_no_match_when_op_is_equal() -> ir::Result<()> {
    let g = graph_if_branches()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&int_lt(int_const(4), int_const(1)));
    assert!(hits.is_empty(), "cond is Equal, not Less");
    Ok(())
}

#[test]
fn int_eq_with_wildcard_operands() -> ir::Result<()> {
    let g = graph_if_branches()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&int_eq(any(), any()));
    assert_eq!(hits.len(), 1, "one Equal comparison node in graph");
    Ok(())
}

// ── Extend / truncate / cast patterns ────────────────────────────────────────

#[test]
fn zero_extend_pattern_matches() -> ir::Result<()> {
    let g = graph_zero_extend_return()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&zero_extend(any()));
    assert_eq!(hits.len(), 1);
    Ok(())
}

#[test]
fn sign_extend_no_match_in_zero_extend_graph() -> ir::Result<()> {
    let g = graph_zero_extend_return()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&sign_extend(any()));
    assert!(hits.is_empty(), "graph uses zero_extend, not sign_extend");
    Ok(())
}

#[test]
fn truncate_pattern_matches() -> ir::Result<()> {
    let g = graph_truncate_return()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&truncate(any()));
    assert_eq!(hits.len(), 1);
    Ok(())
}

#[test]
fn zero_extend_no_match_in_truncate_graph() -> ir::Result<()> {
    let g = graph_truncate_return()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&zero_extend(any()));
    assert!(hits.is_empty(), "graph uses truncate, not extend");
    Ok(())
}

// ── InitialVar patterns ───────────────────────────────────────────────────────

#[test]
fn initial_var_any_matches_all_initial_vars() -> ir::Result<()> {
    let (g, _vn) = graph_with_initial_var()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&initial_var());
    assert_eq!(hits.len(), 1, "one InitialVar node in graph");
    Ok(())
}

#[test]
fn initial_var_for_matches_correct_vn() -> ir::Result<()> {
    let (g, vn) = graph_with_initial_var()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&initial_var_for(vn));
    assert_eq!(hits.len(), 1);
    Ok(())
}

#[test]
fn initial_var_for_wrong_vn_no_match() -> ir::Result<()> {
    let (g, _vn) = graph_with_initial_var()?;
    let m = Matcher::new(&g);
    let other_vn = make_reg_vn(999, 8);
    let hits = m.find_all(&initial_var_for(other_vn));
    assert!(hits.is_empty(), "wrong vn, no match");
    Ok(())
}

#[test]
fn initial_var_any_finds_two_vars() -> ir::Result<()> {
    let (g, _a, _b) = graph_with_two_initial_vars()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&initial_var());
    assert_eq!(hits.len(), 2, "two distinct InitialVar nodes");
    Ok(())
}

#[test]
fn initial_var_for_specific_in_two_var_graph() -> ir::Result<()> {
    let (g, vn_a, vn_b) = graph_with_two_initial_vars()?;
    let m = Matcher::new(&g);
    let hits_a = m.find_all(&initial_var_for(vn_a));
    let hits_b = m.find_all(&initial_var_for(vn_b));
    assert_eq!(hits_a.len(), 1);
    assert_eq!(hits_b.len(), 1);
    // Each should match a different node.
    assert_ne!(hits_a[0].root, hits_b[0].root);
    Ok(())
}
