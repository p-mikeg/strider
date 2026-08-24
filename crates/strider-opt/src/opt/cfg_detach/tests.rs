use super::*;
use strider_ir::node::{NodeKind, ValueKind, ValueType};
use strider_ir::{IRBuilderExt, IRWalker};
use strider_ir_test_utils::{RegisterSet, SENTINEL_LIFT_ADDR, reg_vn};

use crate::{DeadBranchElimination, OptCtx};

// Reproduces the post-DBE shape CfgDetach is meant to clean up: redirect the
// constant `If`'s live successor past it and detach the `If`, leaving the dead
// Region predecessor in place.
fn simulate_dbe_redirect_without_strip(
    fg: &mut strider_ir::Function,
    cond: bool,
) -> crate::Result<()> {
    // Matching on the condition value disambiguates nested Ifs and skips any
    // already-detached If from a prior simulate.
    let want_cond_val: u128 = u128::from(cond);
    let if_node = fg
        .graph()
        .all_node_ids()
        .find(|&n| {
            if !matches!(fg.node_kind(n), NodeKind::If) {
                return false;
            }
            let ins = fg.node_inputs(n);
            if ins.len() != 2 {
                return false;
            }
            let cond_producer = fg.value_definition(ins[1]).0;
            let cond_out = fg.node_outputs(cond_producer);
            matches!(fg.node_kind(cond_producer), NodeKind::IntConst(_))
                && cond_out
                    .first()
                    .is_some_and(|&v| fg.int_const_u128(v) == Some(want_cond_val))
        })
        .expect("a live If with the requested constant condition must exist");
    let if_outputs = fg.node_outputs(if_node).to_vec();
    assert_eq!(if_outputs.len(), 2, "If has [ctrl_true, ctrl_false]");
    let ctrl_true = if_outputs[0];
    let ctrl_false = if_outputs[1];
    let live_ctrl = if cond { ctrl_true } else { ctrl_false };
    // If inputs are [ctrl_in, cond]; ctrl_in is the live control to splice in.
    let ctrl_value = fg.node_inputs(if_node)[0];

    // Scoped so the edit ctx's borrow of `fg` ends here (a bare `drop` of a
    // non-`Drop` type trips `clippy::drop_non_drop`).
    {
        let mut edit = crate::EditFunction::new(fg);
        edit.replace_value(live_ctrl, ctrl_value)?;
        edit.kill_node(if_node);
    }
    Ok(())
}

/// Relies on every fixture below fanning strictly more predecessors into the
/// join than into any branch or entry Region.
fn find_join_region(fg: &strider_ir::Function) -> NodeId {
    fg.graph()
        .all_node_ids()
        .filter(|&n| matches!(fg.node_kind(n), NodeKind::Region))
        .max_by_key(|&n| fg.node_inputs(n).len())
        .expect("at least one Region")
}

fn region_phi_token(fg: &strider_ir::Function, region: NodeId) -> strider_ir::node::ValueId {
    fg.node_outputs(region)[1]
}

fn phi_belongs_to_region(fg: &strider_ir::Function, phi: NodeId, region: NodeId) -> bool {
    let inputs = fg.node_inputs(phi);
    if inputs.is_empty() {
        return false;
    }
    inputs[0] == region_phi_token(fg, region)
}

fn find_var_phi_of_region(
    fg: &strider_ir::Function,
    region: NodeId,
    var: rsleigh::Vn,
) -> Option<NodeId> {
    fg.graph().all_node_ids().find(|&n| {
        matches!(fg.node_kind(n), NodeKind::Phi)
            && fg.get_vn_for_value(fg.node_outputs(n)[0]) == Some(var)
            && phi_belongs_to_region(fg, n, region)
    })
}

fn find_mem_phi_of_region(fg: &strider_ir::Function, region: NodeId) -> Option<NodeId> {
    fg.graph().all_node_ids().find(|&n| {
        matches!(fg.node_kind(n), NodeKind::MemPhi) && phi_belongs_to_region(fg, n, region)
    })
}

/// `if(cond_val) { return 1; } else { return 2; }`
fn make_if_fn(cond_val: bool) -> crate::Result<strider_ir::Function> {
    let mut b = strider_ir_test_utils::empty_builder()?;
    let entry = b.create_region_all()?;
    let true_region = b.create_region_all()?;
    let false_region = b.create_region_all()?;

    b.set_entry_region_all(entry)?;
    b.set_region(entry);
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
    let cond = b.build_boolean_const(cond_val);
    b.build_if(cond, true_region, false_region)?;

    b.set_region(true_region);
    let true_val = b.build_int_const(1u64, strider_ir::ValueType::I64)?;
    b.build_return(Some(true_val), &[])?;

    b.set_region(false_region);
    let false_val = b.build_int_const(2u64, strider_ir::ValueType::I64)?;
    b.build_return(Some(false_val), &[])?;
    b.set_lift_addr(None);

    b.build()
}

/// The dead branch has no downstream join, so it falls fully out of the
/// reachable graph. CfgDetach only visits validator-reachable Regions, so it
/// leaves the orphan alone (keeping its dangling input); orphans are harmless
/// and never swept.
#[test]
fn cfg_detach_removes_dead_region_pred_after_dbe() -> crate::Result<()> {
    let mut fg = make_if_fn(false)?;

    // cond = false, so the true branch (If output 0) is dead. Capture the dead
    // Region before teardown.
    let if_node = fg
        .graph()
        .all_node_ids()
        .find(|&n| matches!(fg.node_kind(n), NodeKind::If))
        .expect("If node must exist");
    let dead_ctrl = fg.node_outputs(if_node)[0];
    let dead_region = fg
        .graph()
        .value_uses(dead_ctrl)
        .map(|(n, _)| n)
        .find(|&n| matches!(fg.node_kind(n), NodeKind::Region))
        .expect("dead branch Region must consume the If's dead control output");

    crate::pipeline::run_one(&DeadBranchElimination, &mut fg, &mut OptCtx::new(None))?;
    crate::pipeline::run_one(&CfgDetach, &mut fg, &mut OptCtx::new(None))?;

    let reachable_regions: Vec<_> = fg
        .walk()
        .filter(|&n| matches!(fg.node_kind(n), NodeKind::Region))
        .collect();
    assert!(
        !reachable_regions.contains(&dead_region),
        "dead Region must be unreachable from entry after DBE + CfgDetach"
    );
    assert_eq!(
        reachable_regions.len(),
        2,
        "entry and the live branch Region must remain reachable"
    );
    strider_ir::validate::validate(&fg)
        .map_err(|e| anyhow::anyhow!("post-teardown validation failed: {e:?}"))?;
    Ok(())
}

/// Isolates CfgDetach from DBE: graft a ctrl edge from a disconnected node onto
/// a branch Region and check the pass strips it on its own.
#[test]
fn cfg_detach_isolated_removes_unreachable_predecessor_slot() -> crate::Result<()> {
    let mut fg = make_if_fn(true)?;

    let if_node = fg
        .graph()
        .all_node_ids()
        .find(|&n| matches!(fg.node_kind(n), NodeKind::If))
        .expect("If node must exist");
    let if_outputs = fg.node_outputs(if_node).to_vec();
    assert_eq!(if_outputs.len(), 2);
    let ctrl_false = if_outputs[1];
    let false_region = fg
        .graph()
        .value_uses(ctrl_false)
        .map(|(n, _)| n)
        .find(|&n| matches!(fg.node_kind(n), NodeKind::Region))
        .expect("false_region must be a Region consumer of ctrl_false");

    assert_eq!(fg.node_inputs(false_region).len(), 1);

    // No ctrl input, so the ghost is unreachable from entry.
    let ghost_region = fg.graph_mut().create_node(
        NodeKind::Region,
        [],
        [ValueKind::Control, ValueKind::PhiToken],
    );
    let ghost_ctrl_value = fg.node_outputs(ghost_region)[0];

    fg.graph_mut()
        .add_node_input(false_region, ghost_ctrl_value);
    assert_eq!(
        fg.node_inputs(false_region).len(),
        2,
        "false_region should now have 2 ctrl inputs after surgery"
    );

    let result = crate::pipeline::run_one(&CfgDetach, &mut fg, &mut OptCtx::new(None))?;
    assert!(
        result.changed(),
        "CfgDetach must report Changed when it removes an unreachable predecessor"
    );
    assert_eq!(
        fg.node_inputs(false_region).len(),
        1,
        "false_region must drop back to 1 ctrl input after CfgDetach"
    );

    Ok(())
}

/// A join carrying BOTH a VarPhi and a MemPhi: the dead control slot and both
/// matching value slots must go, and the result must still validate.
#[test]
fn cfg_detach_collapses_var_and_mem_phi_then_validates() -> crate::Result<()> {
    let var = reg_vn(0x1000, 8);
    let mut b = RegisterSet::new().tracked(var).arg(var).build_fn()?;
    let entry = b.create_region_all()?;
    let true_r = b.create_region_all()?;
    let false_r = b.create_region_all()?;
    let join = b.create_region_all()?;
    b.set_entry_region_all(entry)?;

    b.set_region(entry);
    let cond = b.build_boolean_const(false);
    b.build_if(cond, true_r, false_r)?;

    // The CallOther is only there to advance the memory chain.
    b.set_region(true_r);
    let v_t = b.build_int_const(1u64, ValueType::I64)?;
    b.write_variable(&var, v_t)?;
    let (call_t, _) = b.build_call_other_abi(
        0,
        "cpuid",
        &[],
        &strider_target::BuiltCallOtherAbi {
            implicit_reads: Vec::new(),
            implicit_writes: Vec::new(),
            clobbers_memory: false,
            no_return: false,
        },
        None,
        false,
    )?;
    let mem_t = b.function().node_outputs(call_t)[1];
    b.advance_cur_region_memory(mem_t)?;
    b.build_branch(join)?;

    b.set_region(false_r);
    let v_f = b.build_int_const(2u64, ValueType::I64)?;
    b.write_variable(&var, v_f)?;
    let (call_f, _) = b.build_call_other_abi(
        0,
        "cpuid",
        &[],
        &strider_target::BuiltCallOtherAbi {
            implicit_reads: Vec::new(),
            implicit_writes: Vec::new(),
            clobbers_memory: false,
            no_return: false,
        },
        None,
        false,
    )?;
    let mem_f = b.function().node_outputs(call_f)[1];
    b.advance_cur_region_memory(mem_f)?;
    b.build_branch(join)?;

    // The read forces a VarPhi at the join.
    b.set_region(join);
    let merged = b.read_variable(&var)?;
    b.build_return(Some(merged), &[])?;
    b.set_lift_addr(None);

    let mut fg = b.build()?;

    let join_node = find_join_region(&fg);
    let var_phi = find_var_phi_of_region(&fg, join_node, var).expect("VarPhi at join");
    let mem_phi = find_mem_phi_of_region(&fg, join_node).expect("MemPhi at join");
    assert_eq!(
        fg.node_inputs(var_phi).len(),
        3,
        "VarPhi: token + 2 values before detach"
    );
    assert_eq!(
        fg.node_inputs(mem_phi).len(),
        3,
        "MemPhi: token + 2 values before detach"
    );

    // cond=false, so the true branch is left reachable only via the detached If.
    simulate_dbe_redirect_without_strip(&mut fg, false)?;

    let result = crate::pipeline::run_one(&CfgDetach, &mut fg, &mut OptCtx::new(None))?;
    assert!(result.changed(), "CfgDetach must report Changed");

    assert_eq!(
        fg.node_inputs(var_phi).len(),
        2,
        "VarPhi: token + 1 value after CfgDetach"
    );
    assert_eq!(
        fg.node_inputs(mem_phi).len(),
        2,
        "MemPhi: token + 1 value after CfgDetach"
    );

    let true_r_node = fg
        .graph()
        .all_node_ids()
        .find(|&n| matches!(fg.node_kind(n), NodeKind::Region) && fg.node_inputs(n).is_empty())
        .expect("dead-branch Region with 0 ctrl inputs");
    assert_eq!(fg.node_inputs(true_r_node).len(), 0);

    strider_ir::validate::validate(&fg)
        .map_err(|e| anyhow::anyhow!("post-CfgDetach validation failed: {e:?}"))?;
    Ok(())
}

/// A join whose only phi is the MemPhi.
#[test]
fn cfg_detach_collapses_mem_phi_only_then_validates() -> crate::Result<()> {
    let mut b = strider_ir_test_utils::empty_builder()?;
    let entry = b.create_region_all()?;
    let true_r = b.create_region_all()?;
    let false_r = b.create_region_all()?;
    let join = b.create_region_all()?;
    b.set_entry_region_all(entry)?;

    b.set_region(entry);
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
    let cond = b.build_boolean_const(false);
    b.build_if(cond, true_r, false_r)?;

    b.set_region(true_r);
    let (call_t, _) = b.build_call_other_abi(
        0,
        "cpuid",
        &[],
        &strider_target::BuiltCallOtherAbi {
            implicit_reads: Vec::new(),
            implicit_writes: Vec::new(),
            clobbers_memory: false,
            no_return: false,
        },
        None,
        false,
    )?;
    let mem_t = b.function().node_outputs(call_t)[1];
    b.advance_cur_region_memory(mem_t)?;
    b.build_branch(join)?;

    b.set_region(false_r);
    let (call_f, _) = b.build_call_other_abi(
        0,
        "cpuid",
        &[],
        &strider_target::BuiltCallOtherAbi {
            implicit_reads: Vec::new(),
            implicit_writes: Vec::new(),
            clobbers_memory: false,
            no_return: false,
        },
        None,
        false,
    )?;
    let mem_f = b.function().node_outputs(call_f)[1];
    b.advance_cur_region_memory(mem_f)?;
    b.build_branch(join)?;

    b.set_region(join);
    b.build_return(None, &[])?;
    b.set_lift_addr(None);

    let mut fg = b.build()?;

    let join_node = find_join_region(&fg);
    let mem_phi = find_mem_phi_of_region(&fg, join_node).expect("MemPhi at join");
    assert_eq!(
        fg.node_inputs(mem_phi).len(),
        3,
        "MemPhi: token + 2 values before detach"
    );

    simulate_dbe_redirect_without_strip(&mut fg, false)?;

    let result = crate::pipeline::run_one(&CfgDetach, &mut fg, &mut OptCtx::new(None))?;
    assert!(result.changed(), "CfgDetach must report Changed");

    assert_eq!(
        fg.node_inputs(mem_phi).len(),
        2,
        "MemPhi: token + 1 value after CfgDetach"
    );

    strider_ir::validate::validate(&fg)
        .map_err(|e| anyhow::anyhow!("post-CfgDetach validation failed: {e:?}"))?;
    Ok(())
}

/// Two dead predecessors on a 3-way join: both slots must go in one run.
#[test]
fn cfg_detach_removes_two_dead_predecessors_then_validates() -> crate::Result<()> {
    let var = reg_vn(0x1000, 8);
    let mut b = RegisterSet::new().tracked(var).arg(var).build_fn()?;
    let entry = b.create_region_all()?;
    let r_a = b.create_region_all()?; // outer-true
    let mid = b.create_region_all()?; // outer-false -> inner if
    let r_b = b.create_region_all()?; // inner-true
    let r_c = b.create_region_all()?; // inner-false
    let join = b.create_region_all()?;
    b.set_entry_region_all(entry)?;

    b.set_region(entry);
    let cond_outer = b.build_boolean_const(false);
    b.build_if(cond_outer, r_a, mid)?;

    b.set_region(r_a);
    let v_a = b.build_int_const(1u64, ValueType::I64)?;
    b.write_variable(&var, v_a)?;
    b.build_branch(join)?;

    b.set_region(mid);
    let cond_inner = b.build_boolean_const(true);
    b.build_if(cond_inner, r_b, r_c)?;

    b.set_region(r_b);
    let v_b = b.build_int_const(2u64, ValueType::I64)?;
    b.write_variable(&var, v_b)?;
    b.build_branch(join)?;

    b.set_region(r_c);
    let v_c = b.build_int_const(3u64, ValueType::I64)?;
    b.write_variable(&var, v_c)?;
    b.build_branch(join)?;

    b.set_region(join);
    let merged = b.read_variable(&var)?;
    b.build_return(Some(merged), &[])?;
    b.set_lift_addr(None);

    let mut fg = b.build()?;

    let join_node = find_join_region(&fg);
    assert_eq!(fg.node_inputs(join_node).len(), 3, "3-way join");
    let var_phi = find_var_phi_of_region(&fg, join_node, var).expect("VarPhi at join");
    assert_eq!(fg.node_inputs(var_phi).len(), 4, "token + 3 values");

    // Outer cond=false kills r_a; inner cond=true kills r_c.
    simulate_dbe_redirect_without_strip(&mut fg, false)?; // outer If
    simulate_dbe_redirect_without_strip(&mut fg, true)?; // inner If

    let result = crate::pipeline::run_one(&CfgDetach, &mut fg, &mut OptCtx::new(None))?;
    assert!(result.changed(), "CfgDetach must report Changed");

    assert_eq!(
        fg.node_inputs(join_node).len(),
        1,
        "join Region drops 3→1 ctrl inputs"
    );
    assert_eq!(
        fg.node_inputs(var_phi).len(),
        2,
        "VarPhi drops 3→1 values (token + 1)"
    );

    strider_ir::validate::validate(&fg)
        .map_err(|e| anyhow::anyhow!("post-CfgDetach validation failed: {e:?}"))?;
    Ok(())
}

/// Pins the iteration-set choice: a control-dead Region stays reachable through
/// backward-data edges (its VarPhi still feeds the live join phi until the slot
/// goes), so iterating `walk()` rather than the control-only set is what lets
/// the pass reach it at all.
#[test]
fn cfg_detach_visits_control_dead_but_data_reachable_region() -> crate::Result<()> {
    let var = reg_vn(0x1000, 8);
    let mut b = RegisterSet::new().tracked(var).arg(var).build_fn()?;
    let entry = b.create_region_all()?;
    let true_r = b.create_region_all()?;
    let false_r = b.create_region_all()?;
    let join = b.create_region_all()?;
    b.set_entry_region_all(entry)?;

    b.set_region(entry);
    let cond = b.build_boolean_const(false);
    b.build_if(cond, true_r, false_r)?;

    b.set_region(true_r);
    let v_t = b.build_int_const(1u64, ValueType::I64)?;
    b.write_variable(&var, v_t)?;
    b.build_branch(join)?;

    b.set_region(false_r);
    let v_f = b.build_int_const(2u64, ValueType::I64)?;
    b.write_variable(&var, v_f)?;
    b.build_branch(join)?;

    b.set_region(join);
    let merged = b.read_variable(&var)?;
    b.build_return(Some(merged), &[])?;
    b.set_lift_addr(None);

    let mut fg = b.build()?;

    simulate_dbe_redirect_without_strip(&mut fg, false)?;

    let entry_node = fg.entry();
    let ctrl_reach = strider_ir::walk::cfg_reachable(fg.graph(), entry_node);
    let walk_set: Vec<NodeId> = fg.walk().collect();
    // true_r is the Region whose sole producer (the If's ctrl_true) is now
    // detached.
    let dead_region = fg
        .graph()
        .all_node_ids()
        .filter(|&n| matches!(fg.node_kind(n), NodeKind::Region))
        .find(|&n| {
            let ins = fg.node_inputs(n);
            ins.len() == 1 && !ctrl_reach.contains(fg.value_definition(ins[0]).0)
        })
        .expect("a control-dead Region predecessor must exist");
    assert!(
        !ctrl_reach.contains(dead_region),
        "dead Region is NOT in the control-reachable set"
    );
    assert!(
        walk_set.contains(&dead_region),
        "dead Region IS in the general graph walk (data-reachable)"
    );

    let result = crate::pipeline::run_one(&CfgDetach, &mut fg, &mut OptCtx::new(None))?;
    assert!(result.changed(), "CfgDetach must report Changed");
    assert_eq!(
        fg.node_inputs(dead_region).len(),
        0,
        "control-dead Region's predecessor was removed"
    );

    strider_ir::validate::validate(&fg)
        .map_err(|e| anyhow::anyhow!("post-CfgDetach validation failed: {e:?}"))?;
    Ok(())
}
