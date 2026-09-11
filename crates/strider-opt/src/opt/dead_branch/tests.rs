use super::*;
use strider_ir::node::{NodeId, NodeKind, ValueType};
use strider_ir::{IRBuilderExt, IRWalker};
use strider_ir_test_utils::IrWalkerEx;
use strider_ir_test_utils::{RegisterSet, SENTINEL_LIFT_ADDR, reg_vn};

use crate::{CfgDetach, ConstantFold, OptCtx, OptimizerPipeline, PhiCollapse, RegionCollapse};

fn count_regions_with_n_inputs(fg: &strider_ir::Graph, n: usize) -> usize {
    fg.all_node_ids()
        .filter(|&node| {
            matches!(fg.node_kind(node), NodeKind::Region) && fg.node_inputs(node).len() == n
        })
        .count()
}

/// Uses the same entry-rooted reachability as the validator.
fn reachable_regions(fg: &strider_ir::Function) -> Vec<NodeId> {
    fg.walk()
        .filter(|&n| matches!(fg.node_kind(n), NodeKind::Region))
        .collect()
}

/// DBE then CfgDetach, in the order the destructive pipeline uses.  A dead
/// branch with no downstream join is never visited by CfgDetach and stays in
/// the arena as an unreachable orphan.
fn destructive_teardown(fg: &mut strider_ir::Function) -> Result<()> {
    crate::pipeline::run_one(&DeadBranchElimination, fg, &mut OptCtx::new(None))?;
    crate::pipeline::run_one(&CfgDetach, fg, &mut OptCtx::new(None))?;
    Ok(())
}

fn make_if_fn(cond_val: bool) -> Result<strider_ir::Function> {
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

/// The condition is the proof for taking the arm, so the surviving control
/// source must carry its asm-fingerprint before the condition cone is culled.
#[test]
fn dead_branch_absorbs_condition_fingerprint() -> Result<()> {
    const COND_ADDR: u64 = 0xC0DE_0001;

    let mut b = strider_ir_test_utils::empty_builder()?;
    let entry = b.create_region_all()?;
    let true_region = b.create_region_all()?;
    let false_region = b.create_region_all()?;

    b.set_entry_region_all(entry)?;
    b.set_region(entry);
    // Only the condition carries this address; everything else carries the
    // sentinel, so an absorbed COND_ADDR can only have come from it.
    b.set_lift_addr(Some(COND_ADDR));
    let cond = b.build_boolean_const(true);
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
    b.build_if(cond, true_region, false_region)?;

    b.set_region(true_region);
    let tv = b.build_int_const(1u64, ValueType::I64)?;
    b.build_return(Some(tv), &[])?;
    b.set_region(false_region);
    let fv = b.build_int_const(2u64, ValueType::I64)?;
    b.build_return(Some(fv), &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    // Capture the survivor before the fold; DBE kills the If itself.
    let if_node = fg
        .walk()
        .find(|&n| matches!(fg.node_kind(n), NodeKind::If))
        .expect("if");
    let survivor = fg.producer(fg.node_inputs(if_node)[0]);
    assert!(
        !fg.side_tables()
            .asm_fingerprint(survivor)
            .contains(&COND_ADDR),
        "precondition: the control source must not already carry the condition's addr"
    );

    crate::pipeline::run_one(&DeadBranchElimination, &mut fg, &mut OptCtx::new(None))?;

    assert!(
        fg.side_tables()
            .asm_fingerprint(survivor)
            .contains(&COND_ADDR),
        "DBE must absorb the condition's asm-fingerprint into the surviving \
         control source (proof of why the branch was taken); got {:?}",
        fg.side_tables().asm_fingerprint(survivor)
    );
    Ok(())
}

/// The unique Region consuming the If's dead control output.  Output index 0
/// is the true edge, 1 the false edge.
fn dead_branch_region(fg: &strider_ir::Function, dead_output_index: usize) -> NodeId {
    let if_node = fg
        .graph()
        .all_node_ids()
        .find(|&n| matches!(fg.node_kind(n), NodeKind::If))
        .expect("If node must exist");
    let dead_ctrl = fg.node_outputs(if_node)[dead_output_index];
    fg.graph()
        .value_uses(dead_ctrl)
        .map(|(n, _)| n)
        .find(|&n| matches!(fg.node_kind(n), NodeKind::Region))
        .expect("dead branch Region must consume the If's dead control output")
}

#[test]
fn dead_branch_false() -> Result<()> {
    let mut fg = make_if_fn(false)?;

    // entry, true-branch, false-branch.
    assert_eq!(count_regions_with_n_inputs(fg.graph(), 1), 3);

    // cond = false, so the true branch (output 0) is dead.
    let dead_region = dead_branch_region(&fg, 0);

    let result = crate::pipeline::run_one(&DeadBranchElimination, &mut fg, &mut OptCtx::new(None))?;
    assert!(result.changed());
    crate::pipeline::run_one(&CfgDetach, &mut fg, &mut OptCtx::new(None))?;

    // Unreachability is the outcome that matters; the dead Region keeps its
    // stale inputs in the arena.
    assert!(
        !reachable_regions(&fg).contains(&dead_region),
        "dead branch Region must be unreachable from entry after teardown"
    );
    assert_eq!(
        reachable_regions(&fg).len(),
        2,
        "entry and live branch Region must remain reachable"
    );
    // Orphans are tolerated.
    strider_ir::validate::validate(&fg)
        .map_err(|e| anyhow::anyhow!("post-teardown validation failed: {e:?}"))?;
    Ok(())
}

#[test]
fn dead_branch_true() -> Result<()> {
    let mut fg = make_if_fn(true)?;

    assert_eq!(count_regions_with_n_inputs(fg.graph(), 1), 3);

    // cond = true, so the false branch (output 1) is dead.
    let dead_region = dead_branch_region(&fg, 1);

    let result = crate::pipeline::run_one(&DeadBranchElimination, &mut fg, &mut OptCtx::new(None))?;
    assert!(result.changed());
    crate::pipeline::run_one(&CfgDetach, &mut fg, &mut OptCtx::new(None))?;

    assert!(
        !reachable_regions(&fg).contains(&dead_region),
        "dead (false) branch Region must be unreachable from entry after teardown"
    );
    assert_eq!(
        reachable_regions(&fg).len(),
        2,
        "entry and live (true) branch Region must remain reachable"
    );
    strider_ir::validate::validate(&fg)
        .map_err(|e| anyhow::anyhow!("post-teardown validation failed: {e:?}"))?;
    Ok(())
}

#[test]
fn dead_branch_non_const_no_change() -> Result<()> {
    let mut fg = {
        let mut b = strider_ir_test_utils::empty_builder()?;
        let entry = b.create_region_all()?;
        let true_r = b.create_region_all()?;
        let false_r = b.create_region_all()?;
        b.set_entry_region_all(entry)?;
        b.set_region(entry);
        b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
        // `true & false` stays a node until ConstantFold runs; this test runs
        // DeadBranchElimination alone, so the cond reaches it as a node.
        let t = b.build_boolean_const(true);
        let f = b.build_boolean_const(false);
        let cond = b.build_int_binary_operation(
            t,
            f,
            strider_ir::IntBinaryOp::And,
            strider_ir::node::ValueType::I1,
        )?;
        b.build_if(cond, true_r, false_r)?;
        b.set_region(true_r);
        b.build_return(None, &[])?;
        b.set_region(false_r);
        b.build_return(None, &[])?;
        b.set_lift_addr(None);
        b.build()?
    };

    assert!(
        !crate::pipeline::run_one(&DeadBranchElimination, &mut fg, &mut OptCtx::new(None))?
            .changed()
    );
    Ok(())
}

/// `if(true)` nested in the live branch of an outer `if(true)`: the full
/// destructive pipeline must eliminate both.
#[test]
fn nested_if_true_eliminated() -> Result<()> {
    let mut b = strider_ir_test_utils::empty_builder()?;
    let entry = b.create_region_all()?;
    let outer_t = b.create_region_all()?;
    let outer_f = b.create_region_all()?;
    let inner_t = b.create_region_all()?;
    let inner_f = b.create_region_all()?;
    b.set_entry_region_all(entry)?;

    b.set_region(entry);
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
    let outer_cond = b.build_boolean_const(true);
    b.build_if(outer_cond, outer_t, outer_f)?;

    b.set_region(outer_t);
    let inner_cond = b.build_boolean_const(true);
    b.build_if(inner_cond, inner_t, inner_f)?;

    b.set_region(outer_f);
    b.build_return(None, &[])?;
    b.set_region(inner_t);
    let v = b.build_int_const(1u64, strider_ir::ValueType::I64)?;
    b.build_return(Some(v), &[])?;
    b.set_region(inner_f);
    b.build_return(None, &[])?;
    b.set_lift_addr(None);

    let mut fg = b.build()?;

    let mut pipeline = OptimizerPipeline::new();
    pipeline.add(ConstantFold::new());
    pipeline.add(DeadBranchElimination);
    pipeline.add(CfgDetach);
    pipeline.add(PhiCollapse);
    pipeline.add(RegionCollapse);
    pipeline.run(&mut fg, &mut OptCtx::new(None))?;

    let if_count = fg.count_kind(|k| matches!(k, NodeKind::If));
    assert_eq!(if_count, 0, "both If nodes must be eliminated");
    Ok(())
}

/// The dead control output wired into the same `Region` at two slots.  The
/// branch has no downstream join, so CfgDetach never visits it; the orphaned
/// multi-slot Region is harmless residue as long as it leaves the reachable
/// graph and validation still passes.
///
/// Multi-dead-slot removal on a *reachable* join is pinned separately by
/// `cfg_detach::tests::cfg_detach_removes_two_dead_predecessors_then_validates`.
#[test]
fn dead_branch_handles_dead_ctrl_wired_at_multiple_slots() -> Result<()> {
    let mut fg = make_if_fn(true)?;

    // ctrl_false is the dead output when cond = true.
    let if_node = fg
        .graph()
        .all_node_ids()
        .find(|&n| matches!(fg.node_kind(n), NodeKind::If))
        .expect("expected an If node");
    let if_outputs: Vec<_> = fg.node_outputs(if_node).to_vec();
    assert_eq!(if_outputs.len(), 2, "If must have 2 control outputs");
    let ctrl_false = if_outputs[1];

    let consumers: Vec<_> = fg.graph().value_uses(ctrl_false).collect();
    assert_eq!(
        consumers.len(),
        1,
        "ctrl_false should have exactly one consumer in the standard make_if_fn shape"
    );
    let false_region = consumers[0].0;
    assert!(matches!(fg.node_kind(false_region), NodeKind::Region));

    fg.graph_mut().add_node_input(false_region, ctrl_false);
    let pre_inputs: Vec<_> = fg.node_inputs(false_region).into_iter().collect();
    assert_eq!(pre_inputs.len(), 2);
    assert_eq!(
        pre_inputs[0], ctrl_false,
        "slot 0 must be ctrl_false (original)"
    );
    assert_eq!(
        pre_inputs[1], ctrl_false,
        "slot 1 must be ctrl_false (added duplicate)"
    );

    destructive_teardown(&mut fg)?;

    assert!(
        !reachable_regions(&fg).contains(&false_region),
        "dead false-branch Region must be unreachable from entry after teardown"
    );
    strider_ir::validate::validate(&fg)
        .map_err(|e| anyhow::anyhow!("post-teardown validation failed: {e:?}"))?;
    Ok(())
}

/// The dead branch's `CallOther` advances memory into the join's `MemPhi`.  DBE
/// detaches unconditionally, so only the CfgDetach + PhiCollapse follow-up
/// restores a valid graph.
#[test]
fn dead_branch_with_non_region_dead_consumer() -> Result<()> {
    let mut fg = {
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
        // Gives the join's MemPhi a non-trivial mem-input from the dead branch.
        let (call_node, _) = b.build_call_other_abi(
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
        let mem_value = b.function().node_outputs(call_node)[1];
        b.advance_cur_region_memory(mem_value)?;
        b.build_branch(join)?;

        b.set_region(false_r);
        b.build_branch(join)?;

        b.set_region(join);
        b.build_return(None, &[])?;
        b.set_lift_addr(None);
        b.build()?
    };

    crate::pipeline::run_one(&DeadBranchElimination, &mut fg, &mut OptCtx::new(None))?;
    crate::pipeline::run_one(&CfgDetach, &mut fg, &mut OptCtx::new(None))?;
    crate::pipeline::run_one(&PhiCollapse, &mut fg, &mut OptCtx::new(None))?;

    strider_ir::validate::validate(&fg)
        .map_err(|e| anyhow::anyhow!("post-teardown validation failed: {e:?}"))?;
    Ok(())
}

/// A VarPhi at a 2-input join must lose exactly the dead predecessor's slot.
#[test]
fn var_phi_loses_dead_slot() -> Result<()> {
    let var = reg_vn(0x1000, 8);
    let mut b = RegisterSet::new().tracked(var).arg(var).build_fn()?;
    let entry = b.create_region_all()?;
    let true_r = b.create_region_all()?;
    let false_r = b.create_region_all()?;
    let join = b.create_region_all()?;
    b.set_entry_region_all(entry)?;

    b.set_region(entry);
    let cond = b.build_boolean_const(true);
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
    // Reached via the Return: the builder also emits per-block single-pred
    // VarPhis, and this test wants the 2-input join phi.
    let join_phi = fg.producer(
        fg.node_inputs(
            fg.graph()
                .all_node_ids()
                .find(|&n| matches!(fg.node_kind(n), NodeKind::Return))
                .expect("Return present"),
        )[2],
    );
    assert!(
        matches!(fg.node_kind(join_phi), NodeKind::Phi),
        "Return must read a VarPhi"
    );
    assert_eq!(
        fg.node_inputs(join_phi).len(),
        3,
        "token + 2 values pre-teardown"
    );

    destructive_teardown(&mut fg)?;

    // 1 token + 1 live value.
    let phi_inputs = fg.node_inputs(join_phi);
    assert_eq!(
        phi_inputs.len(),
        2,
        "phi must have exactly 1 live value after teardown"
    );
    Ok(())
}

#[test]
fn dead_switch_const_address_keeps_matching_arm() -> Result<()> {
    let mut b = strider_ir_test_utils::empty_builder()?;
    let entry = b.create_region_all()?;
    let a0 = b.create_region_all()?;
    let a1 = b.create_region_all()?;
    b.set_entry_region_all(entry)?;
    b.set_region(entry);
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
    let addr = b.build_int_const(0x1020u64, strider_ir::ValueType::I64)?; // == arm 1's case
    b.build_switch(addr, &[(a0, 0x1000), (a1, 0x1020)])?;
    b.set_region(a0);
    b.build_return(None, &[])?;
    b.set_region(a1);
    b.build_return(None, &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;
    let n_switch_before = fg
        .graph()
        .all_node_ids()
        .filter(|&n| matches!(fg.node_kind(n), NodeKind::Switch))
        .count();
    assert_eq!(n_switch_before, 1);
    let result = crate::pipeline::run_one(&DeadBranchElimination, &mut fg, &mut OptCtx::new(None))?;
    assert!(result.changed(), "const-address switch must fold");
    // `kill_node` leaves the folded Switch in the arena, so assert
    // unreachability via `walk()` rather than an arena-wide count.
    let n_switch_after = fg
        .walk()
        .filter(|&n| matches!(fg.node_kind(n), NodeKind::Switch))
        .count();
    assert_eq!(n_switch_after, 0, "switch unreachable after fold");
    Ok(())
}

#[test]
fn dead_switch_non_const_address_no_change() -> Result<()> {
    let var = reg_vn(0x1000, 8);
    let mut b = RegisterSet::new().tracked(var).arg(var).build_fn()?;
    let entry = b.create_region_all()?;
    let a0 = b.create_region_all()?;
    let a1 = b.create_region_all()?;
    b.set_entry_region_all(entry)?;
    b.set_region(entry);
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
    let addr = b.read_variable(&var)?;
    b.build_switch(addr, &[(a0, 0x1000), (a1, 0x1020)])?;
    b.set_region(a0);
    b.build_return(None, &[])?;
    b.set_region(a1);
    b.build_return(None, &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    let result = crate::pipeline::run_one(&DeadBranchElimination, &mut fg, &mut OptCtx::new(None))?;
    assert!(!result.changed(), "non-const-address switch must not fold");
    let n_switch_after = fg
        .graph()
        .all_node_ids()
        .filter(|&n| matches!(fg.node_kind(n), NodeKind::Switch))
        .count();
    assert_eq!(n_switch_after, 1, "switch must survive");
    Ok(())
}

/// The dead arm is the loop's ONLY exit, reaching its `Return` through a plain
/// `Region`.  Folding makes the cycle exit-free with no `Unreachable` anywhere,
/// and the optimizer's own validation then rejects the graph it produced.
#[test]
fn dead_arm_carrying_the_only_loop_exit_keeps_the_branch() -> Result<()> {
    let mut b = strider_ir_test_utils::empty_builder()?;
    let entry = b.create_region_all()?;
    let header = b.create_region_all()?;
    let body = b.create_region_all()?;
    let mid = b.create_region_all()?;
    let exit = b.create_region_all()?;
    b.set_entry_region_all(entry)?;
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));

    b.set_region(entry);
    b.build_branch(header)?;

    b.set_region(header);
    // Proven by an earlier fold, so the loop reads as infinite.
    let cond = b.build_boolean_const(true);
    b.build_if(cond, body, mid)?;

    b.set_region(body);
    b.build_branch(header)?;

    b.set_region(mid);
    b.build_branch(exit)?;

    b.set_region(exit);
    b.build_return(None, &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    let result = crate::pipeline::run_one(&DeadBranchElimination, &mut fg, &mut OptCtx::new(None))?;
    strider_ir::validate::validate(&fg)?;
    assert!(
        !result.changed(),
        "folding away a loop's only exit must not happen"
    );
    assert!(
        fg.walk().any(|n| matches!(fg.node_kind(n), NodeKind::If)),
        "the If must survive"
    );
    Ok(())
}

/// A dead arm feeding an `Unreachable` is a liveness anchor, so the branch
/// must not fold.
#[test]
fn dead_arm_feeding_an_unreachable_sink_keeps_the_branch() -> Result<()> {
    let mut fg = make_if_fn(true)?;

    let if_node = fg
        .graph()
        .all_node_ids()
        .find(|&n| matches!(fg.node_kind(n), NodeKind::If))
        .expect("make_if_fn builds an If");
    // cond = true, so output 1 (the false arm) is dead.
    let dead_ctrl = fg.node_outputs(if_node)[1];

    let (dead_region, slot) = fg
        .graph()
        .value_uses(dead_ctrl)
        .next()
        .expect("the dead arm feeds its Region");
    fg.graph_mut().remove_node_input(dead_region, slot);
    fg.graph_mut()
        .create_node(NodeKind::Unreachable, [dead_ctrl], []);

    let result = crate::pipeline::run_one(&DeadBranchElimination, &mut fg, &mut OptCtx::new(None))?;

    assert!(
        !result.changed(),
        "a branch whose dead arm anchors an Unreachable must not fold"
    );
    assert!(
        fg.graph().all_node_ids().any(|n| n == if_node),
        "the If must survive"
    );
    assert!(
        fg.graph()
            .all_node_ids()
            .any(|n| matches!(fg.node_kind(n), NodeKind::Unreachable)),
        "the Unreachable sink must survive"
    );
    Ok(())
}

/// `header: If(true) -> body / mid`, with `body` closing the loop or falling
/// through to the exit.  Both shapes have identical node ids.
fn make_const_header_loop(back_edge: bool) -> Result<strider_ir::Function> {
    let mut b = strider_ir_test_utils::empty_builder()?;
    let entry = b.create_region_all()?;
    let header = b.create_region_all()?;
    let body = b.create_region_all()?;
    let mid = b.create_region_all()?;
    let exit = b.create_region_all()?;
    b.set_entry_region_all(entry)?;
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));

    b.set_region(entry);
    b.build_branch(header)?;

    b.set_region(header);
    let cond = b.build_boolean_const(true);
    b.build_if(cond, body, mid)?;

    b.set_region(body);
    b.build_branch(if back_edge { header } else { exit })?;

    b.set_region(mid);
    b.build_branch(exit)?;

    b.set_region(exit);
    b.build_return(None, &[])?;
    b.set_lift_addr(None);
    b.build()
}

/// The escape memo is keyed by `NodeId`, so one left standing from a previous
/// sweep answers about a different graph.  The acyclic twin folds and fills it
/// with the very ids the loop then asks about.
#[test]
fn escape_memo_does_not_survive_into_the_next_sweep() -> Result<()> {
    let mut acyclic = make_const_header_loop(false)?;
    assert!(
        crate::pipeline::run_one(&DeadBranchElimination, &mut acyclic, &mut OptCtx::new(None))?
            .changed(),
        "the acyclic twin must fold, filling the memo"
    );

    let mut looping = make_const_header_loop(true)?;
    let result =
        crate::pipeline::run_one(&DeadBranchElimination, &mut looping, &mut OptCtx::new(None))?;
    strider_ir::validate::validate(&looping)?;
    assert!(
        !result.changed(),
        "folding away a loop's only exit must not happen"
    );
    Ok(())
}

/// Every constant `If` funnels its LIVE arm into one shared exit-free spin,
/// and reaches a `Return` only through the DEAD arms chained gate to gate.
/// `escaping_nodes` never crosses a dead arm, so no gate is in the escape set
/// and each root falls through to the exact walk; the walk from any gate's
/// live arm covers the spin plus every earlier gate. Without a shared verdict
/// that is one whole-CFG traversal per gate.
#[test]
fn a_chain_of_gates_over_one_spin_walks_the_cfg_once() -> Result<()> {
    const N: usize = 64;
    let mut b = strider_ir_test_utils::empty_builder()?;
    let entry = b.create_region_all()?;
    let gates: Vec<_> = (0..N)
        .map(|_| b.create_region_all())
        .collect::<Result<Vec<_>>>()?;
    let spin_head = b.create_region_all()?;
    let spin_tail = b.create_region_all()?;
    let exit = b.create_region_all()?;
    b.set_entry_region_all(entry)?;
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));

    b.set_region(entry);
    b.build_branch(gates[0])?;

    for (i, &gate) in gates.iter().enumerate() {
        b.set_region(gate);
        let cond = b.build_boolean_const(true);
        let dead_arm = if i + 1 < N { gates[i + 1] } else { exit };
        b.build_if(cond, spin_head, dead_arm)?;
    }

    b.set_region(spin_head);
    b.build_branch(spin_tail)?;
    b.set_region(spin_tail);
    b.build_branch(gates[0])?;

    b.set_region(exit);
    b.build_return(None, &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    super::FULL_WALKS.with(|c| c.set(0));
    let result = crate::pipeline::run_one(&DeadBranchElimination, &mut fg, &mut OptCtx::new(None))?;
    assert!(
        !result.changed(),
        "every gate's live arm spins forever, so none may fold"
    );
    let walks = super::FULL_WALKS.with(std::cell::Cell::get);
    assert!(
        walks <= 1,
        "{N} gates over one spin must share a single walk, ran {walks}"
    );
    Ok(())
}

/// A chain of 64 constant-condition diamonds sharing one `Return`: every
/// branch is answered from the per-sweep escape set, so the whole-CFG walk
/// that used to run per branch never runs at all.
#[test]
fn constant_diamond_chain_never_walks_the_whole_cfg() -> Result<()> {
    const N: usize = 64;
    let mut b = strider_ir_test_utils::empty_builder()?;
    let merges: Vec<_> = (0..=N)
        .map(|_| b.create_region_all())
        .collect::<Result<Vec<_>>>()?;
    let arms: Vec<_> = (0..N)
        .map(|_| Ok((b.create_region_all()?, b.create_region_all()?)))
        .collect::<Result<Vec<_>>>()?;
    b.set_entry_region_all(merges[0])?;
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
    for (i, &(t, f)) in arms.iter().enumerate() {
        b.set_region(merges[i]);
        let cond = b.build_boolean_const(true);
        b.build_if(cond, t, f)?;
        b.set_region(t);
        b.build_branch(merges[i + 1])?;
        b.set_region(f);
        b.build_branch(merges[i + 1])?;
    }
    b.set_region(merges[N]);
    let v = b.build_int_const(7u64, ValueType::I64)?;
    b.build_return(Some(v), &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    super::FULL_WALKS.with(|c| c.set(0));
    assert!(
        crate::pipeline::run_one(&DeadBranchElimination, &mut fg, &mut OptCtx::new(None))?
            .changed(),
        "every constant branch must fold"
    );
    assert_eq!(
        super::FULL_WALKS.with(std::cell::Cell::get),
        0,
        "the escape set must answer every branch"
    );
    Ok(())
}
