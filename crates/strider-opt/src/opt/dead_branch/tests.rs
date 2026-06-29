use super::*;
use strider_ir::node::{NodeId, NodeKind, ValueType};
use strider_ir_test_utils::IrWalkerEx;
use strider_ir::{IRBuilderExt, IRWalker};
use strider_ir_test_utils::{RegisterSet, SENTINEL_LIFT_ADDR, reg_vn};

use crate::pipeline::OptimizerTestExt;
use crate::{CfgDetach, ConstantFold, OptCtx, OptimizerPipeline, PhiCollapse, RegionCollapse};

// Helper: count Region nodes with N ctrl inputs.
fn count_regions_with_n_inputs(fg: &strider_ir::Graph, n: usize) -> usize {
    fg.all_node_ids()
        .filter(|&node| {
            matches!(fg.node_kind(node), NodeKind::Region) && fg.node_inputs(node).len() == n
        })
        .count()
}

/// Collect every Region reachable from the function entry via the general
/// graph walk (the same reachability the validator uses).
fn reachable_regions(fg: &strider_ir::Function) -> Vec<NodeId> {
    fg.walk()
        .filter(|&n| matches!(fg.node_kind(n), NodeKind::Region))
        .collect()
}

/// Run the destructive teardown (DBE → CfgDetach) directly, mirroring the
/// order the destructive pipeline uses.  DBE folds + detaches the constant
/// If; CfgDetach severs dead `Region`-predecessor slots that stay
/// data-reachable.  Any node left fully unreachable (e.g. a dead branch
/// with no downstream join) keeps its inputs but is simply unreachable
/// from entry — orphans don't affect correctness, so they are not swept.
fn destructive_teardown(fg: &mut strider_ir::Function) -> Result<()> {
    DeadBranchElimination.run_one(fg, &mut OptCtx::new(None))?;
    CfgDetach.run_one(fg, &mut OptCtx::new(None))?;
    Ok(())
}

/// Build a function with `if(cond)`, two branches each ending in `return`.
fn make_if_fn(cond_val: bool) -> Result<strider_ir::Function> {
    let mut b = strider_ir_test_utils::empty_builder()?;
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

/// Proof-completeness: the **condition** is part of the proof for killing a
/// dead branch, so after `DeadBranchElimination` folds `if(const)`, the
/// surviving control source must carry the condition's asm-fingerprint.
/// Without the absorb, the condition cone is culled and its asm is lost.
#[test]
fn dead_branch_absorbs_condition_fingerprint() -> Result<()> {
    const COND_ADDR: u64 = 0xC0DE_0001;

    let mut b = strider_ir_test_utils::empty_builder()?;
    let entry = b.create_region()?;
    let true_region = b.create_region()?;
    let false_region = b.create_region()?;

    b.set_entry_region(entry)?;
    b.set_region(entry);
    // The CONDITION carries a distinct address; the `If` + control carry the
    // sentinel — so an absorbed COND_ADDR on the survivor can only have come
    // from the condition.
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

    // The surviving control source is the producer of the If's control input;
    // capture it before the fold (the If itself is killed by DBE).
    let if_node = fg
        .walk()
        .find(|&n| matches!(fg.node_kind(n), NodeKind::If))
        .expect("if");
    let survivor = fg.producer(fg.node_inputs(if_node)[0]);
    assert!(
        !fg.asm_fingerprint(survivor).contains(&COND_ADDR),
        "precondition: the control source must not already carry the condition's addr"
    );

    DeadBranchElimination.run_one(&mut fg, &mut OptCtx::new(None))?;

    assert!(
        fg.asm_fingerprint(survivor).contains(&COND_ADDR),
        "DBE must absorb the condition's asm-fingerprint into the surviving \
         control source (proof of why the branch was taken); got {:?}",
        fg.asm_fingerprint(survivor)
    );
    Ok(())
}

// ── End-state tests (DBE + CfgDetach) ──────────────────────────────────────

/// Identify the dead-branch Region before teardown: it is the unique
/// Region consuming the If's dead control output (index 0 = true output,
/// index 1 = false output).
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

    // Before: three Region nodes with 1 ctrl input each
    // (entry, true-branch, false-branch).
    assert_eq!(count_regions_with_n_inputs(fg.graph(), 1), 3);

    // cond = false → the true branch (If output index 0) is dead.
    let dead_region = dead_branch_region(&fg, 0);

    // DBE alone reports Changed (it folds the constant If).
    let result = DeadBranchElimination.run_one(&mut fg, &mut OptCtx::new(None))?;
    assert!(result.changed());
    // CfgDetach severs the dead predecessor slot of any data-reachable join.
    CfgDetach.run_one(&mut fg, &mut OptCtx::new(None))?;

    // The meaningful outcome: the dead branch Region is no longer reachable
    // from entry (the live branch was redirected past the folded If).  Its
    // inputs are NOT swept — orphans don't affect correctness — but it must
    // not appear in the reachable graph.
    assert!(
        !reachable_regions(&fg).contains(&dead_region),
        "dead branch Region must be unreachable from entry after teardown"
    );
    // Entry and the live (false) branch Region remain reachable.
    assert_eq!(
        reachable_regions(&fg).len(),
        2,
        "entry and live branch Region must remain reachable"
    );
    // The graph still validates (orphans are tolerated).
    strider_ir::validate::validate(&fg)
        .map_err(|e| anyhow::anyhow!("post-teardown validation failed: {e:?}"))?;
    Ok(())
}

#[test]
fn dead_branch_true() -> Result<()> {
    let mut fg = make_if_fn(true)?;

    assert_eq!(count_regions_with_n_inputs(fg.graph(), 1), 3);

    // cond = true → the false branch (If output index 1) is dead.
    let dead_region = dead_branch_region(&fg, 1);

    let result = DeadBranchElimination.run_one(&mut fg, &mut OptCtx::new(None))?;
    assert!(result.changed());
    CfgDetach.run_one(&mut fg, &mut OptCtx::new(None))?;

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
    // Build if(x) where x is a non-const boolean.
    let mut fg = {
        let mut b = strider_ir_test_utils::empty_builder()?;
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

    // DeadBranchElimination alone should not fire because the condition
    // is a BoolBinaryOp node, not a BoolConst.
    assert!(
        !DeadBranchElimination
            .run_one(&mut fg, &mut OptCtx::new(None))?
            .changed()
    );
    Ok(())
}

/// `if(true)` nested inside the live branch of an outer `if(true)` — the
/// destructive pipeline (ConstantFold + DBE + CfgDetach + PhiCollapse +
/// RegionCollapse) must eliminate both Ifs.
#[test]
fn nested_if_true_eliminated() -> Result<()> {
    let mut b = strider_ir_test_utils::empty_builder()?;
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

/// Edge case: the dead control output of an `If` is wired into the SAME
/// `Region` at *multiple* input slots.  After dead-branch elimination the
/// whole branch is unreachable from entry (it has no downstream join, so
/// CfgDetach never visits it); the meaningful outcome is that the dead
/// Region drops out of the reachable graph and the graph still validates —
/// the orphaned multi-slot Region is harmless residue in the arena.
///
/// (CfgDetach's multi-dead-slot removal on a *reachable* join is pinned
/// separately by `cfg_detach::tests::cfg_detach_removes_two_dead_predecessors_then_validates`.)
///
/// Construction: build the standard `if(true)` skeleton, then wire
/// `ctrl_false` (the dead output) into the false-branch Region a second
/// time via `Graph::add_node_input`.  Run DBE + CfgDetach.
#[test]
fn dead_branch_handles_dead_ctrl_wired_at_multiple_slots() -> Result<()> {
    let mut fg = make_if_fn(true)?;

    // Find the If node and its ctrl_false output (dead when cond=true).
    let if_node = fg
        .graph()
        .all_node_ids()
        .find(|&n| matches!(fg.node_kind(n), NodeKind::If))
        .expect("expected an If node");
    let if_outputs: Vec<_> = fg.node_outputs(if_node).to_vec();
    assert_eq!(if_outputs.len(), 2, "If must have 2 control outputs");
    let ctrl_false = if_outputs[1];

    // Find the false-branch Region (the unique consumer of ctrl_false).
    let consumers: Vec<_> = fg.graph().value_uses(ctrl_false).collect();
    assert_eq!(
        consumers.len(),
        1,
        "ctrl_false should have exactly one consumer in the standard make_if_fn shape"
    );
    let false_region = consumers[0].0;
    assert!(matches!(fg.node_kind(false_region), NodeKind::Region));

    // Wire ctrl_false into the same Region a second time, producing the bad shape.
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

    // DBE folds + detaches the If (so ctrl_false's producer is now detached
    // and control-unreachable); CfgDetach runs but only visits reachable
    // joins.
    destructive_teardown(&mut fg)?;

    // The dead false-branch Region must no longer be reachable from entry,
    // and the graph (with the orphaned Region still in the arena) validates.
    assert!(
        !reachable_regions(&fg).contains(&false_region),
        "dead false-branch Region must be unreachable from entry after teardown"
    );
    strider_ir::validate::validate(&fg)
        .map_err(|e| anyhow::anyhow!("post-teardown validation failed: {e:?}"))?;
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
        let mut b = strider_ir_test_utils::empty_builder()?;
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
        let (call_node, _) = b.build_call_other(
            0,
            "cpuid",
            None,
            &[],
            &strider_target::BuiltCallOtherAbi {
                implicit_reads: Vec::new(),
                implicit_writes: Vec::new(),
                clobbers_memory: false,
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

    // Run the destructive teardown in the pipeline order and validate.
    // DBE detaches the folded If; CfgDetach severs the live↔dead edge;
    // PhiCollapse collapses the now single-pred MemPhi; the final state
    // must be structurally valid.
    DeadBranchElimination.run_one(&mut fg, &mut OptCtx::new(None))?;
    CfgDetach.run_one(&mut fg, &mut OptCtx::new(None))?;
    PhiCollapse.run_one(&mut fg, &mut OptCtx::new(None))?;

    strider_ir::validate::validate(&fg)
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
    // The join VarPhi is the one the Return consumes (the builder also
    // emits per-block single-pred VarPhis; we want the 2-input join phi).
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

    // The join VarPhi should now carry only the live predecessor's value
    // input (length = 1 token + 1 value = 2).
    let phi_inputs = fg.node_inputs(join_phi);
    assert_eq!(
        phi_inputs.len(),
        2,
        "phi must have exactly 1 live value after teardown"
    );
    Ok(())
}
