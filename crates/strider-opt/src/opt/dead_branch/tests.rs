use super::*;
use crate::test_support::{run_pass, run_passes};
use strider_ir::node::{NodeId, NodeKind, ValueType};
use strider_ir::{IRBuilderExt, IRWalker};
use strider_ir_test_utils::IrWalkerEx;
use strider_ir_test_utils::{RegisterSet, SENTINEL_LIFT_ADDR, make_if_fn, reg_vn};

use crate::{ConstantFold, OptCtx, PhiCollapse, RegionCollapse};

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
    let tv = b.build_int_const(1u64, ValueType::int(64))?;
    b.build_return(Some(tv), &[])?;
    b.set_region(false_region);
    let fv = b.build_int_const(2u64, ValueType::int(64))?;
    b.build_return(Some(fv), &[])?;
    let mut fg = b.build()?;

    // Capture the survivor before the fold; DBE kills the If itself.
    let if_node = fg.first_kind(|k| matches!(k, NodeKind::If));
    let survivor = fg.producer(fg.node_inputs(if_node)[0]);
    assert!(
        !fg.side_tables()
            .asm_fingerprint(survivor)
            .contains(&COND_ADDR),
        "precondition: the control source must not already carry the condition's addr"
    );

    run_pass(&DeadBranchElimination, &mut fg)?;

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
    let if_node = fg.first_kind(|k| matches!(k, NodeKind::If));
    let dead_ctrl = fg.node_outputs(if_node)[dead_output_index];
    fg.graph()
        .value_uses(dead_ctrl)
        .map(|(n, _)| n)
        .find(|&n| matches!(fg.node_kind(n), NodeKind::Region))
        .expect("dead branch Region must consume the If's dead control output")
}

/// A constant `If` leaves its dead arm's Region unreachable (stale inputs
/// stay in the arena) and the entry and live Region reachable.
#[test]
fn dead_branch_folds_either_constant() -> Result<()> {
    for cond in [false, true] {
        let mut fg = make_if_fn(cond)?;
        // entry, true-branch, false-branch.
        assert_eq!(count_regions_with_n_inputs(fg.graph(), 1), 3);
        // Output 0 is the true edge, the dead one when `cond` is false.
        let dead_region = dead_branch_region(&fg, usize::from(cond));
        assert!(run_pass(&DeadBranchElimination, &mut fg)?, "{cond}");
        let reachable = reachable_regions(&fg);
        assert!(!reachable.contains(&dead_region), "{cond}");
        assert_eq!(reachable.len(), 2, "{cond}: entry and the live Region");
    }
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
        b.build()?
    };

    assert!(!run_pass(&DeadBranchElimination, &mut fg)?);
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
    let outer_cond = b.build_boolean_const(true);
    b.build_if(outer_cond, outer_t, outer_f)?;

    b.set_region(outer_t);
    let inner_cond = b.build_boolean_const(true);
    b.build_if(inner_cond, inner_t, inner_f)?;

    b.set_region(outer_f);
    b.build_return(None, &[])?;
    b.set_region(inner_t);
    let v = b.build_int_const(1u64, strider_ir::ValueType::int(64))?;
    b.build_return(Some(v), &[])?;
    b.set_region(inner_f);
    b.build_return(None, &[])?;

    let mut fg = b.build()?;

    run_passes(
        &mut fg,
        &[
            &ConstantFold,
            &DeadBranchElimination,
            &PhiCollapse,
            &RegionCollapse,
        ],
    )?;

    let if_count = fg.count_kind(|k| matches!(k, NodeKind::If));
    assert_eq!(if_count, 0, "both If nodes must be eliminated");
    Ok(())
}

/// The dead control output wired into the same `Region` at two slots.  The
/// branch has no downstream join, so the strip never visits it; the orphaned
/// multi-slot Region is harmless residue as long as it leaves the reachable
/// graph and validation still passes.
///
/// Multi-dead-slot removal on a *reachable* join is pinned separately by
/// `strip_tests::strip_removes_two_dead_predecessors_then_validates`.
#[test]
fn dead_branch_handles_dead_ctrl_wired_at_multiple_slots() -> Result<()> {
    let mut fg = make_if_fn(true)?;

    // ctrl_false is the dead output when cond = true.
    let if_node = fg.first_kind(|k| matches!(k, NodeKind::If));
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

    run_pass(&DeadBranchElimination, &mut fg)?;

    assert!(
        !reachable_regions(&fg).contains(&false_region),
        "dead false-branch Region must be unreachable from entry after teardown"
    );
    Ok(())
}

/// The dead branch's `CallOther` advances memory into the join's `MemPhi`.  DBE
/// detaches unconditionally.
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
        let cond = b.build_boolean_const(false);
        b.build_if(cond, true_r, false_r)?;

        b.set_region(true_r);
        // Gives the join's MemPhi a non-trivial mem-input from the dead branch.
        let (call_node, _) = strider_ir_test_utils::Tb::named_call_other(
            &mut b,
            0,
            "cpuid",
            &[],
            &[],
            false,
            false,
        )?;
        let mem_value = b.function().node_outputs(call_node)[1];
        b.advance_cur_region_memory(mem_value)?;
        b.build_branch(join)?;

        b.set_region(false_r);
        b.build_branch(join)?;

        b.set_region(join);
        b.build_return(None, &[])?;
        b.build()?
    };

    // `run_pass` panics on invalid output, so reaching here is the assertion.
    run_pass(&DeadBranchElimination, &mut fg)?;
    run_pass(&PhiCollapse, &mut fg)?;
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
    let v_t = b.build_int_const(1u64, ValueType::int(64))?;
    b.write_variable(&var, v_t)?;
    b.build_branch(join)?;

    b.set_region(false_r);
    let v_f = b.build_int_const(2u64, ValueType::int(64))?;
    b.write_variable(&var, v_f)?;
    b.build_branch(join)?;

    b.set_region(join);
    let merged = b.read_variable(&var)?;
    b.build_return(Some(merged), &[])?;

    let mut fg = b.build()?;
    // Reached via the Return: the builder also emits per-block single-pred
    // VarPhis, and this test wants the 2-input join phi.
    let join_phi = fg.producer(fg.node_inputs(fg.first_kind(|k| matches!(k, NodeKind::Return)))[2]);
    assert!(
        matches!(fg.node_kind(join_phi), NodeKind::Phi),
        "Return must read a VarPhi"
    );
    assert_eq!(
        fg.node_inputs(join_phi).len(),
        3,
        "token + 2 values pre-teardown"
    );

    run_pass(&DeadBranchElimination, &mut fg)?;

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
    let addr = b.build_int_const(0x1020u64, strider_ir::ValueType::int(64))?; // == arm 1's case
    b.build_switch(addr, &[(a0, 0x1000), (a1, 0x1020)])?;
    b.set_region(a0);
    b.build_return(None, &[])?;
    b.set_region(a1);
    b.build_return(None, &[])?;
    let mut fg = b.build()?;
    let n_switch_before = fg
        .graph()
        .all_node_ids()
        .filter(|&n| matches!(fg.node_kind(n), NodeKind::Switch(_)))
        .count();
    assert_eq!(n_switch_before, 1);
    let result = run_pass(&DeadBranchElimination, &mut fg)?;
    assert!(result, "const-address switch must fold");
    // `kill_node` leaves the folded Switch in the arena, so assert
    // unreachability via `walk()` rather than an arena-wide count.
    let n_switch_after = fg
        .walk()
        .filter(|&n| matches!(fg.node_kind(n), NodeKind::Switch(_)))
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
    let addr = b.read_variable(&var)?;
    b.build_switch(addr, &[(a0, 0x1000), (a1, 0x1020)])?;
    b.set_region(a0);
    b.build_return(None, &[])?;
    b.set_region(a1);
    b.build_return(None, &[])?;
    let mut fg = b.build()?;

    let result = run_pass(&DeadBranchElimination, &mut fg)?;
    assert!(!result, "non-const-address switch must not fold");
    let n_switch_after = fg
        .graph()
        .all_node_ids()
        .filter(|&n| matches!(fg.node_kind(n), NodeKind::Switch(_)))
        .count();
    assert_eq!(n_switch_after, 1, "switch must survive");
    Ok(())
}

/// A `Switch` whose constant no arm lists keeps every arm: the fold declines,
/// and the escape set counts none of its outputs dead, so the constant `If`
/// above it folds.
#[test]
fn a_switch_constant_no_arm_lists_has_no_dead_arm() -> Result<()> {
    let mut b = strider_ir_test_utils::empty_builder()?;
    let entry = b.create_region_all()?;
    let dispatch = b.create_region_all()?;
    let a0 = b.create_region_all()?;
    let a1 = b.create_region_all()?;
    let skipped = b.create_region_all()?;
    b.set_entry_region_all(entry)?;
    b.set_region(entry);
    let cond = b.build_boolean_const(true);
    b.build_if(cond, dispatch, skipped)?;
    b.set_region(dispatch);
    let addr = b.build_int_const(0x5000u64, ValueType::int(64))?;
    b.build_switch(addr, &[(a0, 0x1000), (a1, 0x1020)])?;
    for region in [a0, a1, skipped] {
        b.set_region(region);
        b.build_return(None, &[])?;
    }
    let mut fg = b.build()?;

    let result = run_pass(&DeadBranchElimination, &mut fg)?;
    assert!(result, "the constant If must fold");
    assert_eq!(fg.walk_kind(|k| matches!(k, NodeKind::If)).count(), 0);
    assert_eq!(
        fg.walk_kind(|k| matches!(k, NodeKind::Switch(_))).count(),
        1,
        "no arm lists 0x5000, so the Switch stays"
    );
    Ok(())
}

/// Two arms listing the constant: the first is taken, as the reference
/// interpreter's `switch_arm` takes it, and the later one is dead.
#[test]
fn a_switch_constant_two_arms_list_takes_the_first() -> Result<()> {
    use strider_ir_test_utils::interp;
    let mut b = strider_ir_test_utils::empty_builder()?;
    let entry = b.create_region_all()?;
    let arms = [
        (b.create_region_all()?, 0x1000),
        (b.create_region_all()?, 0x1020),
        (b.create_region_all()?, 0x1000),
    ];
    b.set_entry_region_all(entry)?;
    b.set_region(entry);
    let addr = b.build_int_const(0x1000u64, ValueType::int(64))?;
    b.build_switch(addr, &arms)?;
    for (i, &(region, _)) in arms.iter().enumerate() {
        b.set_region(region);
        let v = b.build_int_const(i as u64, ValueType::int(32))?;
        b.build_return(Some(v), &[])?;
    }
    let mut fg = b.build()?;
    let env = interp::Env {
        entry: &|_| strider_ir::UBig::ZERO,
        rom: None,
        seed: 0,
        step_budget: 16,
        private: None,
        keeps: None,
    };
    let Ok(interp::Outcome::Return { values, .. }) = interp::run(&fg, &env) else {
        panic!("the reference run returns");
    };
    assert_eq!(values, [interp::Slot::Known(strider_ir::UBig::ZERO)]);

    assert!(run_pass(&DeadBranchElimination, &mut fg)?);
    assert_eq!(
        fg.walk_kind(|k| matches!(k, NodeKind::Switch(_))).count(),
        0
    );
    assert_eq!(fg.int_const_u128(fg.returned()), Some(0));
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
    let mut fg = b.build()?;

    let result = run_pass(&DeadBranchElimination, &mut fg)?;
    assert!(!result, "folding away a loop's only exit must not happen");
    assert!(
        fg.walk().any(|n| matches!(fg.node_kind(n), NodeKind::If)),
        "the If must survive"
    );
    Ok(())
}

/// A dead arm feeding an `Unreachable` folds like any other: the live arm
/// still reaches its `Return`.
#[test]
fn dead_arm_feeding_an_unreachable_sink_folds() -> Result<()> {
    let mut fg = make_if_fn(true)?;

    let if_node = fg.first_kind(|k| matches!(k, NodeKind::If));
    // cond = true, so output 1 (the false arm) is dead.
    let dead_ctrl = fg.node_outputs(if_node)[1];

    let (dead_region, slot) = fg
        .graph()
        .value_uses(dead_ctrl)
        .next()
        .expect("the dead arm feeds its Region");
    fg.graph_mut().remove_node_input(dead_region, slot);
    let sink = fg
        .graph_mut()
        .create_node(NodeKind::Unreachable, [dead_ctrl], []);
    fg.side_tables_mut()
        .extend_asm_fingerprint(sink, &[SENTINEL_LIFT_ADDR]);

    assert!(run_pass(&DeadBranchElimination, &mut fg)?);
    assert!(
        !fg.walk()
            .any(|n| matches!(fg.node_kind(n), NodeKind::If | NodeKind::Unreachable))
    );
    Ok(())
}

/// `header: If(true) -> body / dead`, `body -> header`, the dead arm the
/// loop's only exit, into an `Unreachable`: folding would leave an exit-free
/// cycle, so the branch stays.
#[test]
fn dead_arm_into_an_unreachable_carrying_the_only_loop_exit_keeps_the_branch() -> Result<()> {
    let mut b = strider_ir_test_utils::empty_builder()?;
    let entry = b.create_region_all()?;
    let header = b.create_region_all()?;
    let body = b.create_region_all()?;
    let dead = b.create_region_all()?;
    b.set_entry_region_all(entry)?;
    b.set_region(entry);
    b.build_branch(header)?;
    b.set_region(header);
    let cond = b.build_boolean_const(true);
    b.build_if(cond, body, dead)?;
    b.set_region(body);
    b.build_branch(header)?;
    b.set_region(dead);
    b.build_unreachable()?;
    let mut fg = b.build()?;

    let result = run_pass(&DeadBranchElimination, &mut fg)?;
    assert!(!result);
    assert!(fg.walk().any(|n| matches!(fg.node_kind(n), NodeKind::If)));
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
    b.build()
}

/// The escape memo is keyed by `NodeId`, so one left standing from a previous
/// sweep answers about a different graph.  The acyclic twin folds and fills it
/// with the very ids the loop then asks about.
#[test]
fn escape_memo_does_not_survive_into_the_next_sweep() -> Result<()> {
    let mut acyclic = make_const_header_loop(false)?;
    assert!(
        run_pass(&DeadBranchElimination, &mut acyclic)?,
        "the acyclic twin must fold, filling the memo"
    );

    let mut looping = make_const_header_loop(true)?;
    let result = run_pass(&DeadBranchElimination, &mut looping)?;
    assert!(!result, "folding away a loop's only exit must not happen");
    Ok(())
}

/// Every constant `If` funnels its LIVE arm into one shared exit-free spin,
/// and reaches a `Return` only through the DEAD arms chained gate to gate.
#[test]
fn a_chain_of_gates_over_one_spin_folds_nothing() -> Result<()> {
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
    let mut fg = b.build()?;

    let result = run_pass(&DeadBranchElimination, &mut fg)?;
    assert!(
        !result,
        "every gate's live arm spins forever, so none may fold"
    );
    Ok(())
}

/// A chain of 64 constant-condition diamonds sharing one `Return`: every
/// branch folds off one escape set for the sweep.
#[test]
fn a_constant_diamond_chain_builds_one_escape_set() -> Result<()> {
    const N: usize = 64;
    let mut b = strider_ir_test_utils::empty_builder()?;
    let merges: Vec<_> = (0..=N)
        .map(|_| b.create_region_all())
        .collect::<Result<Vec<_>>>()?;
    let arms: Vec<_> = (0..N)
        .map(|_| Ok((b.create_region_all()?, b.create_region_all()?)))
        .collect::<Result<Vec<_>>>()?;
    b.set_entry_region_all(merges[0])?;
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
    let v = b.build_int_const(7u64, ValueType::int(64))?;
    b.build_return(Some(v), &[])?;
    let mut fg = b.build()?;

    super::ESCAPE_SETS.with(|c| c.set(0));
    assert!(
        run_pass(&DeadBranchElimination, &mut fg)?,
        "every constant branch must fold"
    );
    assert_eq!(fg.walk_kind(|k| matches!(k, NodeKind::If)).count(), 0);
    assert_eq!(super::ESCAPE_SETS.with(std::cell::Cell::get), 1);
    Ok(())
}

/// A ring of `n` constant `If`s whose only exits are their dead arms, each
/// `exit_len` regions from its `Return`.  `live_on_true` picks which arm stays on the
/// ring.
fn make_constant_exit_ring(
    n: usize,
    exit_len: usize,
    live_on_true: bool,
) -> Result<strider_ir::Function> {
    let mut b = strider_ir_test_utils::empty_builder()?;
    let entry = b.create_region_all()?;
    let heads: Vec<_> = (0..n)
        .map(|_| b.create_region_all())
        .collect::<Result<Vec<_>>>()?;
    let exits: Vec<Vec<_>> = (0..n)
        .map(|_| (0..exit_len).map(|_| b.create_region_all()).collect())
        .collect::<Result<Vec<_>>>()?;
    b.set_entry_region_all(entry)?;
    b.set_region(entry);
    b.build_branch(heads[0])?;
    for i in 0..n {
        b.set_region(heads[i]);
        let cond = b.build_boolean_const(live_on_true);
        let (next, exit) = (heads[(i + 1) % n], exits[i][0]);
        if live_on_true {
            b.build_if(cond, next, exit)?;
        } else {
            b.build_if(cond, exit, next)?;
        }
        for pair in exits[i].windows(2) {
            b.set_region(pair[0]);
            b.build_branch(pair[1])?;
        }
        b.set_region(*exits[i].last().expect("non-empty exit"));
        b.build_return(None, &[])?;
    }
    b.build()
}

/// A ring whose only exits are dead arms never leaves it, so no branch on it
/// folds, whichever arm stays on the ring.
#[test]
fn a_ring_whose_exits_are_all_dead_arms_folds_nothing() -> Result<()> {
    for live_on_true in [false, true] {
        let mut fg = make_constant_exit_ring(8, 2, live_on_true)?;
        assert!(!run_pass(&DeadBranchElimination, &mut fg)?);
        assert_eq!(fg.walk_kind(|k| matches!(k, NodeKind::If)).count(), 8);
    }
    Ok(())
}

/// `h1: If(true) -> h2 / exit`, `h2: If(true) -> h1 / exit`: each live arm
/// escapes only through the other's dead arm, so the loop never exits. Neither
/// folds; folding the first a walk reaches, as a per-root escape walk would,
/// leaves the same infinite loop with one branch fewer.
#[test]
fn branches_inside_a_loop_only_their_dead_arms_leave_stay() -> Result<()> {
    let mut b = strider_ir_test_utils::empty_builder()?;
    let entry = b.create_region_all()?;
    let h1 = b.create_region_all()?;
    let h2 = b.create_region_all()?;
    let x1 = b.create_region_all()?;
    let x2 = b.create_region_all()?;
    b.set_entry_region_all(entry)?;
    b.set_region(entry);
    b.build_branch(h1)?;
    b.set_region(h1);
    let cond = b.build_boolean_const(true);
    b.build_if(cond, h2, x1)?;
    b.set_region(h2);
    let cond = b.build_boolean_const(true);
    b.build_if(cond, h1, x2)?;
    for x in [x1, x2] {
        b.set_region(x);
        b.build_return(None, &[])?;
    }
    let mut fg = b.build()?;

    assert!(!run_pass(&DeadBranchElimination, &mut fg)?);
    assert_eq!(fg.walk_kind(|k| matches!(k, NodeKind::If)).count(), 2);
    Ok(())
}

/// `r0: If(true) -> h / x0`, `h: If(true) -> h / x1`: the branch before the
/// loop takes it on its live arm, and the loop leaves only through its own
/// branch's dead arm, so neither branch folds, the one outside the loop
/// included.
/// Mutation: let a live arm escape through another branch's dead arm.
#[test]
fn a_branch_into_a_loop_only_a_dead_arm_leaves_stays() -> Result<()> {
    let mut b = strider_ir_test_utils::empty_builder()?;
    let r0 = b.create_region_all()?;
    let h = b.create_region_all()?;
    let x0 = b.create_region_all()?;
    let x1 = b.create_region_all()?;
    b.set_entry_region_all(r0)?;
    b.set_region(r0);
    let cond = b.build_boolean_const(true);
    b.build_if(cond, h, x0)?;
    b.set_region(h);
    let cond = b.build_boolean_const(true);
    b.build_if(cond, h, x1)?;
    for x in [x0, x1] {
        b.set_region(x);
        b.build_return(None, &[])?;
    }
    let mut fg = b.build()?;

    assert!(!run_pass(&DeadBranchElimination, &mut fg)?);
    assert_eq!(fg.walk_kind(|k| matches!(k, NodeKind::If)).count(), 2);
    Ok(())
}

#[test]
fn dead_branch_alone_leaves_a_joined_diamond_valid() -> Result<()> {
    let mut fg = strider_ir_test_utils::mem_phi_with_two_stores();
    run_passes(&mut fg, &[&DeadBranchElimination])?;
    assert_eq!(
        fg.count_kind(|k| matches!(k, NodeKind::If)),
        0,
        "the constant If folds"
    );
    Ok(())
}

/// Half-wired mid-pipeline: `r1: If(true) -> a / x`, `a -> h`,
/// `h: If(true) -> b / dangling`, `b -> h`.  The inner branch's dead arm has no
/// consumer, so the loop's only way out is an arm the inner fold removes.
/// Both walks treat that dangling dead arm alike, so neither branch folds.
#[test]
fn a_dangling_dead_arm_is_no_escape() -> Result<()> {
    let mut b = strider_ir_test_utils::empty_builder()?;
    let entry = b.create_region_all()?;
    let r1 = b.create_region_all()?;
    let a = b.create_region_all()?;
    let x = b.create_region_all()?;
    let h = b.create_region_all()?;
    let body = b.create_region_all()?;
    let mid = b.create_region_all()?;
    let exit = b.create_region_all()?;
    b.set_entry_region_all(entry)?;
    b.set_region(entry);
    b.build_branch(r1)?;
    b.set_region(r1);
    let cond = b.build_boolean_const(true);
    b.build_if(cond, a, x)?;
    b.set_region(a);
    b.build_branch(h)?;
    b.set_region(x);
    b.build_branch(exit)?;
    b.set_region(h);
    let cond = b.build_boolean_const(true);
    b.build_if(cond, body, mid)?;
    b.set_region(body);
    b.build_branch(h)?;
    b.set_region(mid);
    b.build_branch(exit)?;
    b.set_region(exit);
    b.build_return(None, &[])?;
    let mut fg = b.build()?;

    // Built second, so the later id.
    let inner = fg
        .graph()
        .all_node_ids()
        .filter(|&n| matches!(fg.node_kind(n), NodeKind::If))
        .last()
        .expect("h ends in an If");
    // cond = true, so output 1 (the false arm) is dead.
    let dead_ctrl = fg.node_outputs(inner)[1];
    let (dead_region, slot) = fg
        .graph()
        .value_uses(dead_ctrl)
        .next()
        .expect("the dead arm feeds mid");
    fg.graph_mut().remove_node_input(dead_region, slot);

    // The strip still drops `exit`'s slot from the orphaned `mid`.
    crate::pipeline::run_one_unvalidated(&DeadBranchElimination, &mut fg, &OptCtx::new(None))?;
    assert_eq!(fg.walk_kind(|k| matches!(k, NodeKind::If)).count(), 2);
    Ok(())
}

thread_local! {
    /// Cached-live nodes a fresh entry walk no longer reaches, as the
    /// post-passes see them.
    static STALE_LIVE: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
}

struct RecordStaleLive;

impl crate::Optimizer for RecordStaleLive {
    fn apply(&self, edit: &mut crate::EditFunction<'_>, _ctx: &OptCtx<'_>) -> Result<()> {
        let reached: entity_utils::DenseEntitySet<NodeId> = edit.walk().collect();
        let stale = edit
            .live_snapshot()
            .iter()
            .filter(|&n| !reached.contains(n))
            .count();
        STALE_LIVE.with(|s| s.set(Some(stale)));
        Ok(())
    }
}

/// `If(true)` whose dead arm loads and returns: once the fold detaches that
/// arm, a post-pass must not see its `Load` or `Return` as live.
#[test]
fn a_folded_arm_is_not_live_for_the_post_passes() -> Result<()> {
    let mut b = strider_ir_test_utils::empty_builder()?;
    let entry = b.create_region_all()?;
    let t = b.create_region_all()?;
    let f = b.create_region_all()?;
    b.set_entry_region_all(entry)?;
    b.set_region(entry);
    let cond = b.build_boolean_const(true);
    b.build_if(cond, t, f)?;
    b.set_region(t);
    b.build_return(None, &[])?;
    b.set_region(f);
    let addr = b.build_int_const(0x40u64, ValueType::int(64))?;
    let loaded = b.build_load(addr, rsleigh::VnSpace::RAM, ValueType::int(64))?;
    b.build_return(Some(loaded), &[])?;
    let mut fg = b.build()?;

    let mut p = crate::default_pipeline();
    p.add_post_pass(&RecordStaleLive);
    STALE_LIVE.with(|s| s.set(None));
    p.run(&mut fg, &OptCtx::new(None))?;
    assert_eq!(fg.count_kind(|k| matches!(k, NodeKind::Load(_))), 0);
    assert_eq!(STALE_LIVE.with(std::cell::Cell::get), Some(0));
    Ok(())
}

/// `R0: if (x<3) RA else RJ; RA: if (x==0) RS else RM; RS: [0x100]=x -> RM;
/// RM: if (x==1) return else RJ; RJ: if (true) unreachable else return`.
/// The fold leaves the `Unreachable` as RJ's only exit; it still takes the
/// merged memory, so RJ keeps its `MemPhi`.
fn store_merged_into_a_folded_trap() -> Result<strider_ir::Function> {
    use strider_ir::IntCmpOp;
    let vn = reg_vn(0x1000, 4);
    let i32t = ValueType::int(32);
    let mut b = RegisterSet::new().tracked(vn).arg(vn).build_fn()?;
    let mut regions = Vec::new();
    for _ in 0..8 {
        regions.push(b.create_region_all()?);
    }
    let [r0, ra, rs, rm, r5, rj, r3, r4] = regions[..] else {
        unreachable!()
    };
    b.set_entry_region_all(r0)?;
    b.set_region(r0);
    let x = b.read_variable(&vn)?;
    let three = b.build_int_const(3u64, i32t)?;
    let c = b.build_int_cmp_operation(x, three, IntCmpOp::Less, i32t)?;
    b.build_if(c, ra, rj)?;
    b.set_region(ra);
    let zero = b.build_int_const(0u64, i32t)?;
    let c = b.build_int_cmp_operation(x, zero, IntCmpOp::Equal, i32t)?;
    b.build_if(c, rs, rm)?;
    b.set_region(rs);
    let addr = b.build_int_const(0x100u64, ValueType::int(64))?;
    b.build_store(addr, x, rsleigh::VnSpace::RAM)?;
    b.build_branch(rm)?;
    b.set_region(rm);
    let one = b.build_int_const(1u64, i32t)?;
    let c = b.build_int_cmp_operation(x, one, IntCmpOp::Equal, i32t)?;
    b.build_if(c, r5, rj)?;
    b.set_region(r5);
    b.build_return(None, &[])?;
    b.set_region(rj);
    let t = b.build_boolean_const(true);
    b.build_if(t, r3, r4)?;
    b.set_region(r3);
    b.build_unreachable()?;
    b.set_region(r4);
    b.build_return(None, &[])?;
    b.set_lift_addr(None);
    b.build()
}

#[test]
fn a_folded_branch_into_a_trap_keeps_the_merged_memory() -> Result<()> {
    let mut fg = store_merged_into_a_folded_trap()?;
    crate::pipeline::run_one(&DeadBranchElimination, &mut fg, &OptCtx::new(None))?;
    let mut fg = store_merged_into_a_folded_trap()?;
    crate::default_pipeline().run(&mut fg, &OptCtx::new(None))?;
    Ok(())
}
