use ir::{
    FunctionBuilder, IntBinaryOp,
    node::NodeOutputType,
};
use pattern::*;

use super::common::*;

// ── Edge-case: constant deduplication ────────────────────────────────────────

#[test]
fn deduplicated_constants_yield_single_match() -> ir::Result<()> {
    // Building two int_const(5, U64) returns the same node due to deduplication.
    let mut b = FunctionBuilder::new(vec![], &[], &[], &[])?;
    let r = b.create_region()?;
    b.set_entry_region(r)?;
    b.set_region(r);
    let _c5a = b.build_int_const(5, NodeOutputType::U64);
    let _c5b = b.build_int_const(5, NodeOutputType::U64); // same node
    let sum = b.build_int_binary_operation(_c5a, _c5b, IntBinaryOp::Add, NodeOutputType::U64)?;
    b.build_return(Some(sum), &[])?;
    let g = b.build().expect("build failed: validator rejected graph");

    let m = Matcher::new(&g);
    // Both const-5 references alias the same node, so int_const(5) finds 1.
    let hits = m.find_all(&int_const(5));
    assert_eq!(hits.len(), 1, "deduplication means only one const-5 node");
    Ok(())
}

#[test]
fn two_different_constants_both_found() -> ir::Result<()> {
    let g = graph_add_return()?; // has 5 and 3
    let m = Matcher::new(&g);
    let h5 = m.find_all(&int_const(5));
    let h3 = m.find_all(&int_const(3));
    assert_eq!(h5.len(), 1);
    assert_eq!(h3.len(), 1);
    assert_ne!(h5[0].root, h3[0].root);
    Ok(())
}

// ── Edge-case: pattern on graph with no matching kind ────────────────────────

#[test]
fn phi_no_match_in_graph_without_variables() -> ir::Result<()> {
    // Without tracked variables, no ControlPhi nodes are emitted.
    let g = graph_add_return()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&phi().into());
    assert!(hits.is_empty(), "no variable → no ControlPhi nodes");
    Ok(())
}

#[test]
fn initial_var_no_match_in_graph_without_variables() -> ir::Result<()> {
    let g = graph_add_return()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&initial_var());
    assert!(hits.is_empty());
    Ok(())
}

// ── get_int_const / get_bool_const helpers ────────────────────────────────────

#[test]
fn get_int_const_returns_value_for_const_binding() -> ir::Result<()> {
    let g = graph_add_return()?;
    let m = Matcher::new(&g);
    let lhs_v = Var::new();
    // add(lhs_v, _): lhs is IntConst(5)
    let hits = m.find_all(&add(var(lhs_v), any()).into());
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].get_int_const(lhs_v, &g), Some(5));
    Ok(())
}

#[test]
fn get_int_const_returns_none_for_non_const_binding() -> ir::Result<()> {
    let g = graph_and_add_return()?;
    let m = Matcher::new(&g);
    let inner_v = Var::new();
    // add(inner_v, 1): inner_v is bound to and(4,7) — not a const node
    let hits = m.find_all(&add(var(inner_v), int_const(1)).into());
    assert_eq!(hits.len(), 1);
    assert_eq!(
        hits[0].get_int_const(inner_v, &g),
        None,
        "and(4,7) is not an IntConst node"
    );
    Ok(())
}

#[test]
fn get_int_const_returns_none_for_unbound_var() -> ir::Result<()> {
    let g = graph_add_return()?;
    let m = Matcher::new(&g);
    let unbound = Var::new();
    // Pattern doesn't use `unbound` at all.
    let hits = m.find_all(&add(any(), any()).into());
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].get_int_const(unbound, &g), None);
    Ok(())
}

#[test]
fn get_int_const_works_for_nested_capture() -> ir::Result<()> {
    // Capture the inner constant of and(4, _rhs_) via a nested pattern.
    let g = graph_and_add_return()?;
    let m = Matcher::new(&g);
    let rhs_v = Var::new();
    let hits = m.find_all(&and(int_const(4), var(rhs_v)).into());
    assert_eq!(hits.len(), 1);
    // rhs_v is IntConst(7)
    assert_eq!(hits[0].get_int_const(rhs_v, &g), Some(7));
    Ok(())
}

#[test]
fn get_bool_const_returns_value_for_bool_binding() -> ir::Result<()> {
    let g = graph_bool_not_return()?;
    let m = Matcher::new(&g);
    let v = Var::new();
    let hits = m.find_all(&bool_not(var(v)));
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].get_bool_const(v, &g), Some(true));
    Ok(())
}

#[test]
fn get_bool_const_returns_none_for_int_binding() -> ir::Result<()> {
    // Binding an int const must not be mistaken for a bool const.
    let g = graph_add_return()?;
    let m = Matcher::new(&g);
    let v = Var::new();
    let hits = m.find_all(&add(var(v), any()).into());
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].get_bool_const(v, &g), None);
    Ok(())
}

#[test]
fn get_int_const_for_store_addr_and_data() -> ir::Result<()> {
    let g = graph_store_then_load()?;
    let m = Matcher::new(&g);
    let addr_v = Var::new();
    let data_v = Var::new();
    let hits = m.find_all(&store().addr(var(addr_v)).data(var(data_v)).into());
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].get_int_const(addr_v, &g), Some(0x200));
    assert_eq!(hits[0].get_int_const(data_v, &g), Some(42));
    Ok(())
}

#[test]
fn get_int_const_for_load_addr() -> ir::Result<()> {
    let g = graph_load_return()?;
    let m = Matcher::new(&g);
    let addr_v = Var::new();
    let hits = m.find_all(&load().addr(var(addr_v)).into());
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].get_int_const(addr_v, &g), Some(0x100));
    Ok(())
}

#[test]
fn get_int_const_for_call_target() -> ir::Result<()> {
    let g = graph_call_return()?;
    let m = Matcher::new(&g);
    let tgt_v = Var::new();
    let hits = m.find_all(&call().target(var(tgt_v)).into());
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].get_int_const(tgt_v, &g), Some(0x1234));
    Ok(())
}
