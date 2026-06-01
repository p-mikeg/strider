use super::*;
use strider_ir::FunctionBuilder;
use strider_ir::node::{NodeKind, NodeOutputType};
use strider_ir_test_utils::{reg_vn, RegisterSet, SENTINEL_LIFT_ADDR};

use crate::opt::pipeline::Optimizer;
use crate::opt::{
    CfgDetach, ConstantFold, DetachUnreachable, OptCtx, OptimizerPipeline, PhiCollapse,
    RegionCollapse,
};

// Helper: count Region nodes with N ctrl inputs.
fn count_regions_with_n_inputs(fg: &strider_ir::Graph, n: usize) -> usize {
    fg.all_node_ids()
        .filter(|&node| {
            matches!(fg.node_kind(node), NodeKind::Region)
                && fg.node_inputs(node).len() == n
        })
        .count()
}

/// Run the destructive teardown (DBE → CfgDetach → DetachUnreachable)
/// directly, mirroring the order the destructive pipeline uses.  DBE folds
/// + detaches the constant If, CfgDetach severs dead `Region`-predecessor
/// slots that stay data-reachable, and DetachUnreachable sweeps the inputs
/// of any node that became fully unreachable (e.g. a dead branch with no
/// downstream join).
fn destructive_teardown(fg: &mut strider_ir::Function) -> Result<()> {
    DeadBranchElimination.optimize(fg, &OptCtx::empty())?;
    CfgDetach.optimize(fg, &OptCtx::empty())?;
    DetachUnreachable.optimize(fg, &OptCtx::empty())?;
    Ok(())
}

/// Build a function with `if(cond)`, two branches each ending in `return`.
fn make_if_fn(cond_val: bool) -> Result<strider_ir::Function> {
    let mut b = FunctionBuilder::empty()?;
    let entry = b.create_region()?;
    let true_region = b.create_region()?;
    let false_region = b.create_region()?;

    b.set_entry_region(entry)?;
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

// ── End-state tests (DBE + CfgDetach) ──────────────────────────────────────

#[test]
fn dead_branch_false() -> Result<()> {
    let mut fg = make_if_fn(false)?;

    // Before: three Region nodes with 1 ctrl input each
    // (entry, true-branch, false-branch).
    assert_eq!(count_regions_with_n_inputs(&fg, 1), 3);

    // DBE alone reports Changed (it folds the constant If).
    let result = DeadBranchElimination.optimize(&mut fg, &OptCtx::empty())?;
    assert!(result.changed());
    // CfgDetach + DetachUnreachable then strip the now-dead predecessor.
    CfgDetach.optimize(&mut fg, &OptCtx::empty())?;
    DetachUnreachable.optimize(&mut fg, &OptCtx::empty())?;

    // After: the dead (true) branch Region has 0 inputs; the entry and live
    // (false) branch Regions each still have 1 input.
    assert_eq!(
        count_regions_with_n_inputs(&fg, 0),
        1,
        "dead branch Region should have 0 inputs after teardown"
    );
    assert_eq!(
        count_regions_with_n_inputs(&fg, 1),
        2,
        "entry and live branch Region should have 1 input"
    );
    Ok(())
}

#[test]
fn dead_branch_true() -> Result<()> {
    let mut fg = make_if_fn(true)?;

    assert_eq!(count_regions_with_n_inputs(&fg, 1), 3);

    let result = DeadBranchElimination.optimize(&mut fg, &OptCtx::empty())?;
    assert!(result.changed());
    CfgDetach.optimize(&mut fg, &OptCtx::empty())?;
    DetachUnreachable.optimize(&mut fg, &OptCtx::empty())?;

    assert_eq!(
        count_regions_with_n_inputs(&fg, 0),
        1,
        "dead (false) branch Region should have 0 inputs after teardown"
    );
    assert_eq!(
        count_regions_with_n_inputs(&fg, 1),
        2,
        "entry and live (true) branch Region should have 1 input"
    );
    Ok(())
}

#[test]
fn dead_branch_non_const_no_change() -> Result<()> {
    // Build if(x) where x is a non-const boolean.
    let mut fg = {
        let mut b = FunctionBuilder::empty()?;
        let entry = b.create_region()?;
        let true_r = b.create_region()?;
        let false_r = b.create_region()?;
        b.set_entry_region(entry)?;
        b.set_region(entry);
        b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
        // Non-constant condition: true & false (booleans are 1-bit ints, so
        // this is `IntBinaryOp::And` at I1 over two I1 IntConsts).  Two nodes
        // combined so it won't be constant at the If level until ConstantFold
        // runs — but we don't run ConstantFold here.
        let t = b.build_boolean_const(true);
        let f = b.build_boolean_const(false);
        let cond = b.build_int_binary_operation(
            t,
            f,
            strider_ir::IntBinaryOp::And,
            strider_ir::node::NodeOutputType::I1,
        )?;
        b.build_if(cond, true_r, false_r)?;
        b.set_region(true_r);
        b.build_return(None, &[])?;
        b.set_region(false_r);
        b.build_return(None, &[])?;
        b.set_lift_addr(None);
        b.build()?
    };

    // DeadBranchElimination alone should not fire because the condition
    // is a BoolBinaryOp node, not a BoolConst.
    assert!(!DeadBranchElimination.optimize(&mut fg, &OptCtx::empty())?.changed());
    Ok(())
}

/// `if(true)` nested inside the live branch of an outer `if(true)` — the
/// destructive pipeline (ConstantFold + DBE + CfgDetach + PhiCollapse +
/// RegionCollapse) must eliminate both Ifs.
#[test]
fn nested_if_true_eliminated() -> Result<()> {
    let mut b = FunctionBuilder::empty()?;
    let entry = b.create_region()?;
    let outer_t = b.create_region()?;
    let outer_f = b.create_region()?;
    let inner_t = b.create_region()?;
    let inner_f = b.create_region()?;
    b.set_entry_region(entry)?;

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
    pipeline.add(ConstantFold);
    pipeline.add(DeadBranchElimination);
    pipeline.add(CfgDetach);
    pipeline.add(PhiCollapse);
    pipeline.add(RegionCollapse);
    pipeline.run(&mut fg, &OptCtx::empty())?;

    let if_count = fg.count_kind(|k| matches!(k, NodeKind::If));
    assert_eq!(if_count, 0, "both If nodes must be eliminated");
    Ok(())
}

/// Edge case: the dead control output of an `If` is wired into the SAME
/// `Region` at *multiple* input slots.  `CfgDetach` (which now owns
/// dead-predecessor removal) removes all such slots.
///
/// Construction: build the standard `if(true)` skeleton, then wire
/// `ctrl_false` (the dead output) into the false-branch Region a second
/// time via `Graph::add_node_input`.  Run DBE (folds + detaches the If)
/// then CfgDetach (removes the now-unreachable predecessor slots).
#[test]
fn dead_branch_handles_dead_ctrl_wired_at_multiple_slots() -> Result<()> {
    let mut fg = make_if_fn(true)?;

    // Find the If node and its ctrl_false output (dead when cond=true).
    let if_node = fg
        .all_node_ids()
        .find(|&n| matches!(fg.node_kind(n), NodeKind::If))
        .expect("expected an If node");
    let if_outputs: Vec<_> = fg.node_outputs(if_node).to_vec();
    assert_eq!(if_outputs.len(), 2, "If must have 2 control outputs");
    let ctrl_false = if_outputs[1];

    // Find the false-branch Region (the unique consumer of ctrl_false).
    let consumers: Vec<_> = fg.output_uses(ctrl_false).collect();
    assert_eq!(
        consumers.len(),
        1,
        "ctrl_false should have exactly one consumer in the standard make_if_fn shape"
    );
    let false_region = consumers[0].0;
    assert!(matches!(fg.node_kind(false_region), NodeKind::Region));

    // Wire ctrl_false into the same Region a second time, producing the bad shape.
    fg.add_node_input(false_region, ctrl_false)?;
    let pre_inputs: Vec<_> = fg.node_inputs(false_region).into_iter().collect();
    assert_eq!(pre_inputs.len(), 2);
    assert_eq!(pre_inputs[0], ctrl_false, "slot 0 must be ctrl_false (original)");
    assert_eq!(pre_inputs[1], ctrl_false, "slot 1 must be ctrl_false (added duplicate)");

    // DBE folds + detaches the If (so ctrl_false's producer is now detached
    // and control-unreachable); CfgDetach then removes both dead slots.
    destructive_teardown(&mut fg)?;

    let post_inputs: Vec<_> = fg.node_inputs(false_region).into_iter().collect();
    assert_eq!(
        post_inputs.len(),
        0,
        "CfgDetach must remove both dead-ctrl wires; got {} remaining input(s) {:?}",
        post_inputs.len(),
        post_inputs,
    );
    Ok(())
}

/// Escape case: a dead-branch subgraph whose data still feeds a live
/// `MemPhi`.  The new DBE detaches the folded `If` unconditionally;
/// `CfgDetach` severs the dead `Region`-predecessor edge, and `PhiCollapse`
/// collapses the resulting single-pred `MemPhi`.  The end-state graph must
/// VALIDATE — this is the soundness `CfgDetach`'s
/// `cfg_detach_collapses_var_and_mem_phi_then_validates` already proved.
///
/// Shape: `if(false) { mem++; } else { } join: return`, with the dead
/// branch's `CallOther` advancing memory into the join's `MemPhi`.
#[test]
fn dead_branch_with_non_region_dead_consumer() -> Result<()> {
    let mut fg = {
        let mut b = FunctionBuilder::empty()?;
        let entry = b.create_region()?;
        let true_r = b.create_region()?;
        let false_r = b.create_region()?;
        let join = b.create_region()?;
        b.set_entry_region(entry)?;

        b.set_region(entry);
        b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
        let cond = b.build_boolean_const(false);
        b.build_if(cond, true_r, false_r)?;

        b.set_region(true_r);
        // Advance memory through a modeled CallOther so the join's MemPhi
        // has a non-trivial mem-input from the (dead) true branch.
        let (call_node, _, _) = b.build_call_other_modeled(0, "cpuid", &[], None, &[], &[], &[])?;
        let mem_out = b.function().node_outputs(call_node)[1];
        b.advance_cur_region_memory(mem_out)?;
        b.build_branch(join)?;

        b.set_region(false_r);
        b.build_branch(join)?;

        b.set_region(join);
        b.build_return(None, &[])?;
        b.set_lift_addr(None);
        b.build()?
    };

    // Run the destructive teardown in the pipeline order and validate.
    // DBE detaches the folded If; CfgDetach severs the live↔dead edge;
    // PhiCollapse collapses the now single-pred MemPhi; the final state
    // must be structurally valid.
    DeadBranchElimination.optimize(&mut fg, &OptCtx::empty())?;
    CfgDetach.optimize(&mut fg, &OptCtx::empty())?;
    PhiCollapse.optimize(&mut fg, &OptCtx::empty())?;

    strider_ir::validate::validate(&fg, fg.entry().unwrap())
        .map_err(|e| anyhow::anyhow!("post-teardown validation failed: {e:?}"))?;
    Ok(())
}

/// A VarPhi at a 2-input join — when the dead branch is removed (DBE +
/// CfgDetach), the phi must lose exactly one input slot (the dead position).
#[test]
fn var_phi_loses_dead_slot() -> Result<()> {
    let var = reg_vn(0x1000, 8);
    let mut b = RegisterSet::new().tracked(var).arg(var).build_fn()?;
    let entry = b.create_region()?;
    let true_r = b.create_region()?;
    let false_r = b.create_region()?;
    let join = b.create_region()?;
    b.set_entry_region(entry)?;

    b.set_region(entry);
    let cond = b.build_boolean_const(true);
    b.build_if(cond, true_r, false_r)?;

    b.set_region(true_r);
    let v_t = b.build_int_const(1u64, NodeOutputType::I64)?;
    b.write_variable(&var, v_t)?;
    b.build_branch(join)?;

    b.set_region(false_r);
    let v_f = b.build_int_const(2u64, NodeOutputType::I64)?;
    b.write_variable(&var, v_f)?;
    b.build_branch(join)?;

    b.set_region(join);
    let merged = b.read_variable(&var)?;
    b.build_return(Some(merged), &[])?;
    b.set_lift_addr(None);

    let mut fg = b.build()?;
    // The join VarPhi is the one the Return consumes (the builder also
    // emits per-block single-pred VarPhis; we want the 2-input join phi).
    let join_phi = fg.node_for_output(
        fg.node_inputs(
            fg.all_node_ids()
                .find(|&n| matches!(fg.node_kind(n), NodeKind::Return))
                .expect("Return present"),
        )[2],
    );
    assert!(
        matches!(fg.node_kind(join_phi), NodeKind::Phi),
        "Return must read a VarPhi"
    );
    assert_eq!(fg.node_inputs(join_phi).len(), 3, "token + 2 values pre-teardown");

    destructive_teardown(&mut fg)?;

    // The join VarPhi should now carry only the live predecessor's value
    // input (length = 1 token + 1 value = 2).
    let phi_inputs = fg.node_inputs(join_phi);
    assert_eq!(phi_inputs.len(), 2, "phi must have exactly 1 live value after teardown");
    Ok(())
}
