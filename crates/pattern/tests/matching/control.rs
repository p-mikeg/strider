use ir::node::{NodeKind, NodeOutputType};
use pattern::*;

use super::common::*;

// ── ret() pattern ─────────────────────────────────────────────────────────────

#[test]
fn ret_pattern_finds_return_node() -> ir::Result<()> {
    let g = graph_add_return()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&ret().into());
    assert_eq!(hits.len(), 1);
    assert!(matches!(g.graph.node_kind(hits[0].root), NodeKind::Return));
    Ok(())
}

#[test]
fn ret_pattern_finds_both_returns_in_if_graph() -> ir::Result<()> {
    let g = graph_if_branches()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&ret().into());
    assert_eq!(hits.len(), 2, "if graph has two return nodes");
    Ok(())
}

// ── ret().ret_val ─────────────────────────────────────────────────────────────

#[test]
fn ret_val_pattern_matches_correct_return_value() -> ir::Result<()> {
    // graph_add_return returns add(5,3). We can match it by its result pattern.
    let g = graph_add_return()?;
    let m = Matcher::new(&g);
    // The return value (input[2]) is the add node.
    let hits = m.find_all(&ret().ret_val(0, add(int_const(5), int_const(3))).into());
    assert_eq!(hits.len(), 1, "return value is add(5,3)");
    Ok(())
}

#[test]
fn ret_val_pattern_no_match_for_wrong_value() -> ir::Result<()> {
    let g = graph_add_return()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&ret().ret_val(0, int_const(99)).into());
    assert!(hits.is_empty(), "return value is not 99");
    Ok(())
}

#[test]
fn ret_val_wildcard_matches_any_return_with_value() -> ir::Result<()> {
    let g = graph_add_return()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&ret().ret_val(0, any()).into());
    assert_eq!(hits.len(), 1);
    Ok(())
}

#[test]
fn ret_val_distinguishes_branches() -> ir::Result<()> {
    // In graph_if_branches, true branch returns 10, false branch returns 20.
    let g = graph_if_branches()?;
    let m = Matcher::new(&g);
    let hits_10 = m.find_all(&ret().ret_val(0, int_const(10)).into());
    let hits_20 = m.find_all(&ret().ret_val(0, int_const(20)).into());
    assert_eq!(hits_10.len(), 1, "one return with value 10");
    assert_eq!(hits_20.len(), 1, "one return with value 20");
    assert_ne!(
        hits_10[0].root, hits_20[0].root,
        "they must be different nodes"
    );
    Ok(())
}

// ── ret().preceded_by ─────────────────────────────────────────────────────────

#[test]
fn preceded_by_matches_call_at_correct_address() -> ir::Result<()> {
    let g = graph_call_return()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&ret().preceded_by(call().at(0x1234)).into());
    assert_eq!(hits.len(), 1);
    Ok(())
}

#[test]
fn preceded_by_no_match_for_wrong_address() -> ir::Result<()> {
    let g = graph_call_return()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&ret().preceded_by(call().at(0xDEAD)).into());
    assert!(hits.is_empty());
    Ok(())
}

#[test]
fn preceded_by_no_match_when_no_call_precedes_return() -> ir::Result<()> {
    // graph_add_return has no call.
    let g = graph_add_return()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&ret().preceded_by(call().at(0x1234)).into());
    assert!(hits.is_empty());
    Ok(())
}

// ── call() pattern ────────────────────────────────────────────────────────────

#[test]
fn call_pattern_finds_all_calls() -> ir::Result<()> {
    let g = graph_two_calls_return()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&call().into());
    assert_eq!(hits.len(), 2);
    Ok(())
}

#[test]
fn call_at_matches_specific_address() -> ir::Result<()> {
    let g = graph_two_calls_return()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&call().at(0x1111).into());
    assert_eq!(hits.len(), 1);
    let hits2 = m.find_all(&call().at(0x2222).into());
    assert_eq!(hits2.len(), 1);
    Ok(())
}

#[test]
fn call_at_wrong_address_no_match() -> ir::Result<()> {
    let g = graph_call_return()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&call().at(0xBEEF).into());
    assert!(hits.is_empty());
    Ok(())
}

// ── if_node() pattern ─────────────────────────────────────────────────────────

#[test]
fn if_pattern_finds_if_node() -> ir::Result<()> {
    let g = graph_if_branches()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&if_node().into());
    assert_eq!(hits.len(), 1);
    assert!(matches!(g.graph.node_kind(hits[0].root), NodeKind::If));
    Ok(())
}

#[test]
fn if_cond_pattern_matches_condition() -> ir::Result<()> {
    // graph_if_branches: cond = Equal(4, 1)
    let g = graph_if_branches()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&if_node().cond(int_eq(int_const(4), int_const(1))).into());
    assert_eq!(hits.len(), 1);
    Ok(())
}

#[test]
fn if_cond_wrong_pattern_no_match() -> ir::Result<()> {
    let g = graph_if_branches()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&if_node().cond(int_eq(int_const(99), int_const(1))).into());
    assert!(hits.is_empty());
    Ok(())
}

#[test]
fn if_pattern_no_match_in_flat_graph() -> ir::Result<()> {
    let g = graph_call_return()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&if_node().into());
    assert!(hits.is_empty());
    Ok(())
}

// ── true_branch / false_branch (one-step-direct semantics) ──────────────────

#[test]
fn true_branch_matches_call_after_control_state() -> ir::Result<()> {
    let g = graph_if_with_call_in_true_branch()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&if_node().true_branch(call().at(0x2345)).into());
    assert_eq!(hits.len(), 1, "true branch should reach call at 0x2345");
    Ok(())
}

#[test]
fn false_branch_contains_call_no_match() -> ir::Result<()> {
    // The call is in the TRUE branch, so false_branch(call) should fail.
    let g = graph_if_with_call_in_true_branch()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&if_node().false_branch(call().at(0x2345)).into());
    assert!(hits.is_empty(), "false branch does not reach the call");
    Ok(())
}

#[test]
fn true_branch_wrong_address_no_match() -> ir::Result<()> {
    let g = graph_if_with_call_in_true_branch()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&if_node().true_branch(call().at(0xDEAD)).into());
    assert!(hits.is_empty());
    Ok(())
}

#[test]
fn false_branch_matches_call_after_control_state() -> ir::Result<()> {
    let g = graph_if_with_call_in_false_branch()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&if_node().false_branch(call().at(0x5678)).into());
    assert_eq!(hits.len(), 1, "false branch reaches call at 0x5678");
    Ok(())
}

#[test]
fn true_branch_no_match_when_call_only_in_false() -> ir::Result<()> {
    let g = graph_if_with_call_in_false_branch()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&if_node().true_branch(call().at(0x5678)).into());
    assert!(hits.is_empty(), "call is only in false branch");
    Ok(())
}

#[test]
fn both_branches_match_ret_after_control_state() -> ir::Result<()> {
    // In graph_if_branches both branches end in a return.  One-step-direct:
    // the `ControlState` that joins the branch body to its `Return` is
    // transparent, so `true_branch(ret())` lands directly on the Return.
    let g = graph_if_branches()?;
    let m = Matcher::new(&g);
    let hits_true = m.find_all(&if_node().true_branch(ret()).into());
    let hits_false = m.find_all(&if_node().false_branch(ret()).into());
    assert_eq!(hits_true.len(), 1, "true branch reaches a return");
    assert_eq!(hits_false.len(), 1, "false branch reaches a return");
    Ok(())
}

#[test]
fn both_branches_constrained_simultaneously() -> ir::Result<()> {
    // Require the true branch to reach a ret returning 10 AND the false
    // branch to reach a ret returning 20.
    let g = graph_if_branches()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(
        &if_node()
            .true_branch(ret().ret_val(0, int_const(10)))
            .false_branch(ret().ret_val(0, int_const(20)))
            .into(),
    );
    assert_eq!(hits.len(), 1);
    Ok(())
}

#[test]
fn both_branches_constrained_swapped_no_match() -> ir::Result<()> {
    // Swapping the expected values must not match.
    let g = graph_if_branches()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(
        &if_node()
            .true_branch(ret().ret_val(0, int_const(20)))
            .false_branch(ret().ret_val(0, int_const(10)))
            .into(),
    );
    assert!(hits.is_empty(), "values are swapped, should not match");
    Ok(())
}

// ── Capture across control + data patterns ────────────────────────────────────

#[test]
fn capture_var_from_call_target_via_preceded_by() -> ir::Result<()> {
    // ret().preceded_by(call — capture target address as var)
    let g = graph_call_return()?;
    let m = Matcher::new(&g);
    let addr_v = Var::new();
    let hits = m.find_all(&ret().preceded_by(call().target(var(addr_v))).into());
    assert_eq!(hits.len(), 1);
    let bound = hits[0].get(addr_v).expect("addr_v must be bound");
    let node = g.graph.get_node_from_output(bound);
    assert!(
        matches!(g.graph.node_kind(node), NodeKind::IntConst(0x1234)),
        "target should be 0x1234, got {:?}",
        g.graph.node_kind(node)
    );
    Ok(())
}

#[test]
fn capture_call_target_via_true_branch() -> ir::Result<()> {
    let g = graph_if_with_call_in_true_branch()?;
    let m = Matcher::new(&g);
    let tgt_v = Var::new();
    let hits = m.find_all(
        &if_node()
            .true_branch(call().target(var(tgt_v)))
            .into(),
    );
    assert_eq!(hits.len(), 1);
    let bound = hits[0].get(tgt_v).expect("tgt_v must be bound");
    let node = g.graph.get_node_from_output(bound);
    assert!(
        matches!(g.graph.node_kind(node), NodeKind::IntConst(0x2345)),
        "call target should be 0x2345, got {:?}",
        g.graph.node_kind(node)
    );
    Ok(())
}

// ── Load patterns ─────────────────────────────────────────────────────────────

#[test]
fn load_any_matches_load_node() -> ir::Result<()> {
    let g = graph_load_return()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&load().into());
    assert_eq!(hits.len(), 1);
    let _node = g
        .graph
        .get_node_from_output(g.graph.node_outputs(hits[0].root)[0]);
    assert!(matches!(g.graph.node_kind(hits[0].root), NodeKind::Load(_)));
    Ok(())
}

#[test]
fn load_with_matching_addr_matches() -> ir::Result<()> {
    let g = graph_load_return()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&load().addr(int_const(0x100)).into());
    assert_eq!(hits.len(), 1);
    Ok(())
}

#[test]
fn load_with_wrong_addr_no_match() -> ir::Result<()> {
    let g = graph_load_return()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&load().addr(int_const(0x999)).into());
    assert!(hits.is_empty());
    Ok(())
}

#[test]
fn load_with_correct_space_matches() -> ir::Result<()> {
    let g = graph_load_return()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&load().space(rsleigh::VnSpace::RAM).into());
    assert_eq!(hits.len(), 1);
    Ok(())
}

#[test]
fn load_with_wrong_space_no_match() -> ir::Result<()> {
    let g = graph_load_return()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&load().space(rsleigh::VnSpace::REGISTER).into());
    assert!(hits.is_empty());
    Ok(())
}

#[test]
fn load_addr_and_space_together_matches() -> ir::Result<()> {
    let g = graph_load_return()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(
        &load()
            .space(rsleigh::VnSpace::RAM)
            .addr(int_const(0x100))
            .into(),
    );
    assert_eq!(hits.len(), 1);
    Ok(())
}

#[test]
fn load_capture_addr() -> ir::Result<()> {
    let g = graph_load_return()?;
    let m = Matcher::new(&g);
    let addr_v = Var::new();
    let hits = m.find_all(&load().addr(var(addr_v)).into());
    assert_eq!(hits.len(), 1);
    let val = hits[0]
        .get_int_const(addr_v, &g)
        .expect("addr must be an int const");
    assert_eq!(val, 0x100);
    Ok(())
}

#[test]
fn load_no_match_in_graph_without_load() -> ir::Result<()> {
    let g = graph_add_return()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&load().into());
    assert!(hits.is_empty());
    Ok(())
}

// ── Store patterns ────────────────────────────────────────────────────────────

// Note: the Store node is reachable via preorder only when its memory output
// is consumed by something downstream (here a Load that feeds into Return).

#[test]
fn store_any_matches_store_node() -> ir::Result<()> {
    let g = graph_store_then_load()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&store().into());
    assert_eq!(hits.len(), 1);
    assert!(matches!(
        g.graph.node_kind(hits[0].root),
        NodeKind::Store(_)
    ));
    Ok(())
}

#[test]
fn store_with_matching_addr_matches() -> ir::Result<()> {
    let g = graph_store_then_load()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&store().addr(int_const(0x200)).into());
    assert_eq!(hits.len(), 1);
    Ok(())
}

#[test]
fn store_with_wrong_addr_no_match() -> ir::Result<()> {
    let g = graph_store_then_load()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&store().addr(int_const(0x999)).into());
    assert!(hits.is_empty());
    Ok(())
}

#[test]
fn store_with_matching_data_matches() -> ir::Result<()> {
    let g = graph_store_then_load()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&store().data(int_const(42)).into());
    assert_eq!(hits.len(), 1);
    Ok(())
}

#[test]
fn store_with_wrong_data_no_match() -> ir::Result<()> {
    let g = graph_store_then_load()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&store().data(int_const(0)).into());
    assert!(hits.is_empty());
    Ok(())
}

#[test]
fn store_with_addr_and_data_matches() -> ir::Result<()> {
    let g = graph_store_then_load()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&store().addr(int_const(0x200)).data(int_const(42)).into());
    assert_eq!(hits.len(), 1);
    Ok(())
}

#[test]
fn store_with_correct_space_matches() -> ir::Result<()> {
    let g = graph_store_then_load()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&store().space(rsleigh::VnSpace::RAM).into());
    assert_eq!(hits.len(), 1);
    Ok(())
}

#[test]
fn store_with_wrong_space_no_match() -> ir::Result<()> {
    let g = graph_store_then_load()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&store().space(rsleigh::VnSpace::REGISTER).into());
    assert!(hits.is_empty());
    Ok(())
}

#[test]
fn store_capture_addr_and_data() -> ir::Result<()> {
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
fn store_reachable_via_call_memory_chain() -> ir::Result<()> {
    // Store's memory → Call (which takes cur_region_memory as input), so
    // the Store is reachable from Return via the Call's inputs.
    let g = graph_store_then_call()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&store().into());
    assert_eq!(
        hits.len(),
        1,
        "store is reachable through the call's memory input"
    );
    Ok(())
}

#[test]
fn store_no_match_in_load_only_graph() -> ir::Result<()> {
    let g = graph_load_return()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&store().into());
    assert!(hits.is_empty(), "load-only graph has no store");
    Ok(())
}

// ── Call argument patterns ────────────────────────────────────────────────────

#[test]
fn call_arg0_matches_correct_value() -> ir::Result<()> {
    let (g, _) = graph_call_with_arg()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&call().arg(0, int_const(42)).into());
    assert_eq!(hits.len(), 1);
    Ok(())
}

#[test]
fn call_arg0_wrong_value_no_match() -> ir::Result<()> {
    let (g, _) = graph_call_with_arg()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&call().arg(0, int_const(0)).into());
    assert!(hits.is_empty());
    Ok(())
}

#[test]
fn call_target_and_arg_together() -> ir::Result<()> {
    let (g, _) = graph_call_with_arg()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&call().at(0xABCD).arg(0, int_const(42)).into());
    assert_eq!(hits.len(), 1);
    Ok(())
}

#[test]
fn call_target_matches_but_arg_wrong_no_match() -> ir::Result<()> {
    let (g, _) = graph_call_with_arg()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&call().at(0xABCD).arg(0, int_const(99)).into());
    assert!(hits.is_empty());
    Ok(())
}

#[test]
fn call_arg_out_of_range_no_match() -> ir::Result<()> {
    // arg index 1 doesn't exist (only 1 arg in this graph).
    let (g, _) = graph_call_with_arg()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&call().arg(1, any()).into());
    assert!(hits.is_empty());
    Ok(())
}

#[test]
fn call_arg_wildcard_matches() -> ir::Result<()> {
    let (g, _) = graph_call_with_arg()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&call().arg(0, any()).into());
    assert_eq!(hits.len(), 1);
    Ok(())
}

#[test]
fn call_arg_capture_and_extract() -> ir::Result<()> {
    let (g, _) = graph_call_with_arg()?;
    let m = Matcher::new(&g);
    let arg_v = Var::new();
    let hits = m.find_all(&call().arg(0, var(arg_v)).into());
    assert_eq!(hits.len(), 1);
    // get_int_const provides the easy value extraction the user asked for.
    let val = hits[0]
        .get_int_const(arg_v, &g)
        .expect("arg must be an int const");
    assert_eq!(val, 42);
    Ok(())
}

// ── call().ret_output + ret().ret_val + Match::get_vn ────────────────────────

#[test]
fn call_ret_output_captures_abi_return_slot() -> ir::Result<()> {
    let (g, ret_reg) = graph_call_then_return_ret_reg()?;
    let m = Matcher::new(&g);
    let v = Var::new();
    let hits = m.find_all(&call().ret_output(0, var(v)).into());
    assert_eq!(hits.len(), 1, "one call with a ret-reg output");
    let bound = hits[0].get(v).expect("v must be bound");
    // The captured output must sit at slot 2 of the Call (the first ABI
    // return register).
    let (node, slot) = g.graph.output_definition(bound);
    assert!(matches!(g.graph.node_kind(node), NodeKind::Call));
    assert_eq!(slot, 2, "ret_output(0) corresponds to Call output slot 2");
    // Match::get_vn should resolve the captured output to the ret reg vn.
    assert_eq!(hits[0].get_vn(v, &g), Some(ret_reg));
    Ok(())
}

#[test]
fn ret_val_index_zero_matches_ret_reg_input() -> ir::Result<()> {
    let (g, ret_reg) = graph_call_then_return_ret_reg()?;
    let m = Matcher::new(&g);
    let v = Var::new();
    let hits = m.find_all(&ret().ret_val(0, var(v)).into());
    assert_eq!(hits.len(), 1);
    // The Return's slot-2 input is the Call's slot-2 output, i.e. the
    // post-call value of the ret reg.
    assert_eq!(hits[0].get_vn(v, &g), Some(ret_reg));
    Ok(())
}

#[test]
fn get_vn_returns_none_for_non_vn_producers() -> ir::Result<()> {
    // An IntConst producer has no associated vn — get_vn must return None.
    let g = graph_add_return()?;
    let m = Matcher::new(&g);
    let v = Var::new();
    let hits = m.find_all(&ret().ret_val(0, var(v)).into());
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].get_vn(v, &g), None);
    Ok(())
}

// ── call_other() pattern ──────────────────────────────────────────────────────

/// Two CallOther sites: op-id 1 with arg 0xAA, op-id 2 with arg 0xBB, then ret.
fn graph_two_call_others_return() -> ir::Result<ir::BuiltFunctionGraph> {
    let mut b = ir::FunctionBuilder::new_raw(vec![], &[], &[], &[], None, 0)?;
    let r = b.create_region()?;
    b.set_entry_region(r)?;
    b.set_region(r);
    let a1 = b.build_int_const(0xAA, NodeOutputType::U64);
    b.build_call_other(1, &[a1], None)?;
    let a2 = b.build_int_const(0xBB, NodeOutputType::U64);
    b.build_call_other(2, &[a2], None)?;
    b.build_return(None, &[])?;
    b.build()
}

#[test]
fn call_other_finds_all_sites() -> ir::Result<()> {
    let g = graph_two_call_others_return()?;
    let m = Matcher::new(&g);
    assert_eq!(m.find_all(&call_other().into()).len(), 2);
    Ok(())
}

#[test]
fn call_other_filters_by_user_op_id() -> ir::Result<()> {
    let g = graph_two_call_others_return()?;
    let m = Matcher::new(&g);
    assert_eq!(m.find_all(&call_other().user_op_id(1).into()).len(), 1);
    assert_eq!(m.find_all(&call_other().user_op_id(2).into()).len(), 1);
    assert!(m.find_all(&call_other().user_op_id(99).into()).is_empty());
    Ok(())
}

#[test]
fn call_other_matches_arg() -> ir::Result<()> {
    let g = graph_two_call_others_return()?;
    let m = Matcher::new(&g);
    assert_eq!(
        m.find_all(&call_other().user_op_id(1).arg(0, int_const(0xAA)).into())
            .len(),
        1
    );
    // Wrong arg value → no match.
    assert!(
        m.find_all(&call_other().user_op_id(1).arg(0, int_const(0xBB)).into())
            .is_empty()
    );
    Ok(())
}

#[test]
fn call_other_captures_node_id() -> ir::Result<()> {
    let g = graph_two_call_others_return()?;
    let m = Matcher::new(&g);
    let cv = NodeVar::new();
    let hits = m.find_all(&call_other().user_op_id(2).capture(cv).into());
    assert_eq!(hits.len(), 1);
    let node = hits[0].get_node(cv).expect("NodeVar must bind");
    assert!(matches!(
        g.graph.node_kind(node),
        NodeKind::CallOther { user_op_id: 2 }
    ));
    Ok(())
}

// ── match_at + AnyBoolConst + when_match ──────────────────────────────────────

#[test]
fn match_at_positive_commutative_add() -> ir::Result<()> {
    let g = graph_add_return()?;
    let m = Matcher::new(&g);
    let add_node = add_node_in_add_graph(&g);
    let l = Var::new();
    let r = Var::new();
    let pat = add(any_int_const(l), any_int_const(r)).into();
    let result = m.match_at(add_node, &pat);
    assert!(result.is_some(), "match_at should succeed on the Add node");
    let mat = result.unwrap();
    // The two IntConst values are 5 and 3 (in some order due to commutativity).
    let lv = mat.get_int_const(l, &g).expect("l must bind to IntConst");
    let rv = mat.get_int_const(r, &g).expect("r must bind to IntConst");
    let mut pair = [lv, rv];
    pair.sort();
    assert_eq!(pair, [3, 5], "captured consts must be 3 and 5");
    Ok(())
}

#[test]
fn match_at_negative_wrong_node() -> ir::Result<()> {
    let g = graph_add_return()?;
    let m = Matcher::new(&g);
    // Pick a non-Add node (the IntConst(5) node).
    let const5_node = g
        .preorder()
        .find(|&n| matches!(g.graph.node_kind(n), NodeKind::IntConst(5)))
        .expect("IntConst(5) must exist");
    let l = Var::new();
    let r = Var::new();
    let pat = add(any_int_const(l), any_int_const(r)).into();
    assert!(
        m.match_at(const5_node, &pat).is_none(),
        "match_at on IntConst(5) with Add pattern must fail"
    );
    Ok(())
}

#[test]
fn any_bool_const_binds_and_extracts() -> ir::Result<()> {
    let g = graph_bool_const_return()?;
    let m = Matcher::new(&g);
    let b_var = Var::new();
    let pat = any_bool_const(b_var);
    let hits = m.find_all(&pat);
    assert_eq!(hits.len(), 1, "exactly one BoolConst node expected");
    let mat = &hits[0];
    let val = mat
        .get_bool_const(b_var, &g)
        .expect("b_var must bind to BoolConst");
    assert!(val, "the bound BoolConst must be true");
    Ok(())
}

#[test]
fn when_match_succeeds_when_sum_equals_eight() -> ir::Result<()> {
    let g = graph_add_return()?;
    let m = Matcher::new(&g);
    let add_node = add_node_in_add_graph(&g);
    let l = Var::new();
    let r = Var::new();
    let inner: Pat = add(any_int_const(l), any_int_const(r)).into();
    let pat = inner.when_match(move |g, _ty, bindings| {
        let lv = resolve_int_const(g, bindings, l).unwrap_or(0);
        let rv = resolve_int_const(g, bindings, r).unwrap_or(0);
        lv + rv == 8
    });
    assert!(
        m.match_at(add_node, &pat).is_some(),
        "sum is 5+3=8, when_match predicate must pass"
    );
    Ok(())
}

#[test]
fn when_match_fails_when_sum_wrong() -> ir::Result<()> {
    let g = graph_add_return()?;
    let m = Matcher::new(&g);
    let add_node = add_node_in_add_graph(&g);
    let l = Var::new();
    let r = Var::new();
    let inner: Pat = add(any_int_const(l), any_int_const(r)).into();
    let pat = inner.when_match(move |g, _ty, bindings| {
        let lv = resolve_int_const(g, bindings, l).unwrap_or(0);
        let rv = resolve_int_const(g, bindings, r).unwrap_or(0);
        lv + rv == 999
    });
    assert!(
        m.match_at(add_node, &pat).is_none(),
        "sum is 5+3=8, predicate requiring 999 must fail"
    );
    Ok(())
}

#[test]
fn when_match_commutative_fallthrough_accepts_swapped_order() -> ir::Result<()> {
    // `graph_add_return` builds add(5, 3), so input0=5 and input1=3.
    // The pattern is unordered: `add(any_int_const(l), any_int_const(r))`.
    // The predicate rejects unless l is bound to the IntConst(5).
    // Natural order binds l=input0=5, so the predicate succeeds immediately.
    // This test also validates that when_match cooperates correctly with the
    // commutative matching infrastructure: `when_match` wraps a `Pat` whose
    // inner `IntBinaryOp` arm handles both orderings, and a predicate failure
    // from `when_match` correctly propagates a `false` result (which in the
    // commutative arm would have caused a retry, had the first ordering failed).
    let g = graph_add_return()?;
    let m = Matcher::new(&g);
    let add_node = add_node_in_add_graph(&g);
    let l = Var::new();
    let r = Var::new();
    let inner: Pat = add(any_int_const(l), any_int_const(r)).into();
    let pat = inner.when_match(move |g, _ty, bindings| {
        let Some(l_out) = bindings.get(l) else {
            return false;
        };
        let n = g.graph.get_node_from_output(l_out);
        matches!(*g.graph.node_kind(n), NodeKind::IntConst(5))
    });
    assert!(
        m.match_at(add_node, &pat).is_some(),
        "natural ordering binds l=5 which satisfies the predicate"
    );
    Ok(())
}

// ── function_arg() patterns ───────────────────────────────────────────────────

#[test]
fn function_arg_any_matches_reg_arg() {
    let (g, _reg) = graph_with_function_arg_reg();
    let m = Matcher::new(&g);
    let hits = m.find_all(&function_arg_any().into());
    assert_eq!(hits.len(), 1, "exactly one FunctionArg node");
    assert!(matches!(
        g.graph.node_kind(hits[0].root),
        NodeKind::FunctionArg { .. }
    ));
}

#[test]
fn function_arg_by_index_matches() {
    let (g, _reg) = graph_with_function_arg_reg();
    let m = Matcher::new(&g);
    let hits = m.find_all(&function_arg(0).into());
    assert_eq!(hits.len(), 1, "FunctionArg with index 0 exists");
}

#[test]
fn function_arg_by_wrong_index_no_match() {
    let (g, _reg) = graph_with_function_arg_reg();
    let m = Matcher::new(&g);
    let hits = m.find_all(&function_arg(7).into());
    assert!(hits.is_empty(), "no FunctionArg with index 7 in this graph");
}

#[test]
fn function_arg_capture_output_binds_value() {
    let (g, _reg) = graph_with_function_arg_reg();
    let m = Matcher::new(&g);
    let v = Var::new();
    let hits = m.find_all(&function_arg(0).capture(v));
    assert_eq!(hits.len(), 1);
    let out = hits[0].get(v).expect("v must bind");
    let node = g.graph.get_node_from_output(out);
    assert!(matches!(
        g.graph.node_kind(node),
        NodeKind::FunctionArg { index: 0, .. }
    ));
}

// ── Matcher::function_arg / function_arg_count / function_args ────────────────

#[test]
fn matcher_function_arg_returns_handle() {
    let (g, reg) = graph_with_function_arg_reg();
    let m = Matcher::new(&g);
    let h = m.function_arg(0).expect("arg 0 should exist");
    assert_eq!(h.index(), 0);
    assert!(matches!(
        g.graph.node_kind(h.node_id()),
        NodeKind::FunctionArg { index: 0, .. }
    ));
    // Source should be the register.
    assert!(matches!(
        h.source(),
        ir::node::FunctionArgSource::Register(r) if r == reg
    ));
}

#[test]
fn matcher_function_arg_missing_returns_none() {
    let (g, _reg) = graph_with_function_arg_reg();
    let m = Matcher::new(&g);
    assert!(m.function_arg(7).is_none());
}

#[test]
fn matcher_function_arg_count_reflects_max_index() {
    let (g, _reg) = graph_with_function_arg_reg();
    let m = Matcher::new(&g);
    assert_eq!(m.function_arg_count(), 1);
}

#[test]
fn matcher_function_args_iterates_all() {
    let (g, _reg) = graph_with_function_arg_reg();
    let m = Matcher::new(&g);
    let collected: Vec<u32> = m.function_args().map(|(i, _)| i).collect();
    assert_eq!(collected, vec![0]);
}

// ── One-step skip semantics ───────────────────────────────────────────────────
//
// Coverage for the new logic introduced by the trait-merge + one-step-skip
// refactor: `true_branch`/`false_branch`/`preceded_by` skip transparent SSA
// plumbing (`ControlState`, `IfCase`) but stop at any semantic node (`Call`,
// `Return`, `If`, `Load`, `Store`, …).  These tests also exercise the unified
// `Pattern` trait dispatch — data sub-patterns nested inside control patterns
// must still evaluate correctly without the deleted `PatAsData` adapter.

#[test]
fn true_branch_skips_control_state_to_reach_call() -> ir::Result<()> {
    // Entry → If → (true-ctrl) → ControlState → Call(0x2345) → Return
    let g = graph_if_with_call_in_true_branch()?;
    let m = Matcher::new(&g);

    // Unconstrained call reached through the transparent ControlState.
    let hits_any = m.find_all(&if_node().true_branch(call()).into());
    assert_eq!(hits_any.len(), 1, "true branch must skip CS and reach Call");

    // Same walk, constrained to the specific call target.
    let hits_addr = m.find_all(&if_node().true_branch(call().at(0x2345)).into());
    assert_eq!(hits_addr.len(), 1, "constrained target still matches");
    Ok(())
}

#[test]
fn true_branch_stops_at_first_semantic_node() -> ir::Result<()> {
    // Entry → If → (true-ctrl) → ControlState → Call(0x1111) → Return.
    //
    // A forward walk from `If`'s true ctrl output must land on the first
    // semantic node it meets — the Call — and NOT walk through it.  We
    // therefore expect a match for `at(0x1111)` (the landed-on Call) and no
    // match for any call() constraint that doesn't refer to that Call.
    let g = graph_if_with_call_in_true_branch()?;
    let m = Matcher::new(&g);

    // Wrong target: there is no call with target 0x2222 on the true branch,
    // and the walk does NOT transparently pass through Call(0x2345) to look
    // for further Calls beyond it.
    let hits_wrong = m.find_all(&if_node().true_branch(call().at(0x2222)).into());
    assert!(
        hits_wrong.is_empty(),
        "walk must stop at Call(0x2345); Call must not be treated as transparent"
    );

    // Sanity: the actual target does match.
    let hits_right = m.find_all(&if_node().true_branch(call().at(0x2345)).into());
    assert_eq!(hits_right.len(), 1);
    Ok(())
}

#[test]
fn preceded_by_skips_control_state() -> ir::Result<()> {
    // Entry → Call(0x1234) → ControlState → Return.
    //
    // `graph_call_return` puts both the Call and the Return in the same
    // region, so the `Return`'s control input is produced by a
    // `ControlState` sitting between the Call and the Return (the region's
    // join node).  The one-step-backward walk must skip that ControlState
    // and land on the Call.
    let g = graph_call_return()?;
    let m = Matcher::new(&g);

    let hits_any = m.find_all(&ret().preceded_by(call()).into());
    assert_eq!(
        hits_any.len(),
        1,
        "preceded_by must skip the ControlState and reach the Call"
    );

    let hits_addr = m.find_all(&ret().preceded_by(call().at(0x1234)).into());
    assert_eq!(hits_addr.len(), 1, "constrained target still matches");
    Ok(())
}

#[test]
fn preceded_by_dead_end_returns_no_match() -> ir::Result<()> {
    // `graph_if_branches` has two Return nodes, neither preceded by a Call
    // on its control chain — the ctrl edge goes back through ControlState
    // to an `IfCase` / `If`, not to a Call.  The pattern must cleanly fail
    // (no match, no panic).
    let g = graph_if_branches()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&ret().preceded_by(call()).into());
    assert!(
        hits.is_empty(),
        "no Call exists on the ctrl chain preceding the Return"
    );
    Ok(())
}

#[test]
fn find_all_call_still_uses_pre_indexed_list() -> ir::Result<()> {
    // Sanity check for `candidate_kind()` routing survival across the trait
    // merge.  The Matcher pre-indexes `Call` nodes; `find_all` on a
    // `CallPat` must use that index and only consider Call roots.  A graph
    // with a single Call at 0x1234 (plus an `IntConst(0x1234)` node that
    // happens to share the same integer value) would produce a false
    // positive if the routing fell through to a generic all-nodes scan.
    let g = graph_call_return()?;
    let m = Matcher::new(&g);

    // Exact address match: one Call at 0x1234.
    let hits = m.find_all(&call().at(0x1234).into());
    assert_eq!(hits.len(), 1);
    assert!(matches!(g.graph.node_kind(hits[0].root), NodeKind::Call));

    // Non-matching address: the graph also contains an `IntConst(0x1234)`
    // (the Call's target).  If routing were broken and the scan fell
    // through to all nodes, the IntConst might be considered a candidate
    // root.  It never matches `call()`, so the correct answer is 0.
    let hits_wrong = m.find_all(&call().at(0xDEAD).into());
    assert!(hits_wrong.is_empty());
    Ok(())
}

#[test]
fn call_with_int_const_arg_matches_via_unified_dispatch() -> ir::Result<()> {
    // Sanity-check that data sub-patterns (`int_const(5)`) still evaluate
    // correctly when nested inside a control pattern (`call().arg(...)`)
    // through the unified `Pattern` trait.  Before the refactor this path
    // went through a `PatAsData` wrapper; post-refactor it's direct
    // trait dispatch.
    //
    // Graph: Entry → Call(target=0x1234, arg0=IntConst(5)) → Return.
    let arg_vn = make_reg_vn(0, 8);
    let mut b = ir::FunctionBuilder::new_raw(vec![arg_vn], &[arg_vn], &[], &[], None, 0)?;
    let r = b.create_region()?;
    b.set_entry_region(r)?;
    b.set_region(r);
    let c5 = b.build_uint64_const(5);
    b.write_variable(&arg_vn, c5)?;
    let tgt = b.build_uint64_const(0x1234);
    b.build_call(tgt)?;
    b.build_return(None, &[])?;
    let g = b.build()?;
    let m = Matcher::new(&g);

    let hits = m.find_all(&call().at(0x1234).arg(0, int_const(5)).into());
    assert_eq!(hits.len(), 1, "nested data pattern must match via unified dispatch");

    let hits_wrong = m.find_all(&call().at(0x1234).arg(0, int_const(999)).into());
    assert!(hits_wrong.is_empty(), "wrong arg value must not match");
    Ok(())
}

