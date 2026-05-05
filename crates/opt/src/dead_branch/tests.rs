use super::*;
use ir::FunctionBuilder;
use ir::node::{NodeKind, NodeOutputType};
use ir::test_utils::reg_vn;

use crate::pipeline::Optimizer;
use crate::{ConstantFold, OptimizerPipeline, RedundantPhis};

// Helper: count ControlState nodes with N ctrl inputs.
fn count_cs_with_n_inputs(fg: &ir::BuiltFunctionGraph, n: usize) -> usize {
    fg.all_node_ids()
        .filter(|&node| {
            matches!(fg.graph.node_kind(node), NodeKind::ControlState)
                && fg.graph.node_inputs(node).len() == n
        })
        .count()
}

/// Build a function with `if(cond)`, two branches each ending in `return`.
fn make_if_fn(cond_val: bool) -> Result<ir::BuiltFunctionGraph> {
    let mut b = FunctionBuilder::empty()?;
    let entry = b.create_region()?;
    let true_region = b.create_region()?;
    let false_region = b.create_region()?;

    b.set_entry_region(entry)?;
    b.set_region(entry);
    let cond = b.build_boolean_const(cond_val);
    b.build_if(cond, true_region, false_region)?;

    b.set_region(true_region);
    let true_val = b.build_int_const(1u64, ir::ValueType::U64)?;
    b.build_return(Some(true_val), &[])?;

    b.set_region(false_region);
    let false_val = b.build_int_const(2u64, ir::ValueType::U64)?;
    b.build_return(Some(false_val), &[])?;

    b.build()
}

// ── Original tests ────────────────────────────────────────────────────────────

#[test]
fn dead_branch_false() -> Result<()> {
    let mut fg = make_if_fn(false)?;

    // Before: three ControlState nodes with 1 ctrl input each
    // (entry, true-branch, false-branch).
    assert_eq!(count_cs_with_n_inputs(&fg, 1), 3);

    let result = DeadBranchElimination.optimize(&mut fg.graph, fg.entry)?;
    assert!(result.changed());

    // After: true region's CS loses its input (dead branch removed).
    // Entry CS and false region's CS each still have 1 input.
    assert_eq!(
        count_cs_with_n_inputs(&fg, 0),
        1,
        "dead branch CS should have 0 inputs"
    );
    assert_eq!(
        count_cs_with_n_inputs(&fg, 1),
        2,
        "entry and live branch CS should have 1 input"
    );
    Ok(())
}

#[test]
fn dead_branch_true() -> Result<()> {
    let mut fg = make_if_fn(true)?;

    assert_eq!(count_cs_with_n_inputs(&fg, 1), 3);

    let result = DeadBranchElimination.optimize(&mut fg.graph, fg.entry)?;
    assert!(result.changed());

    assert_eq!(
        count_cs_with_n_inputs(&fg, 0),
        1,
        "dead (false) branch CS should have 0 inputs"
    );
    assert_eq!(
        count_cs_with_n_inputs(&fg, 1),
        2,
        "entry and live (true) branch CS should have 1 input"
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
        // Non-constant condition: BoolConst(true) & BoolConst(false)
        // (two nodes combined so it won't be constant at the If level until
        // ConstantFold runs — but we don't run ConstantFold here).
        let t = b.build_boolean_const(true);
        let f = b.build_boolean_const(false);
        let cond = b.build_boolean_operation(t, f, ir::BoolBinaryOp::And)?;
        b.build_if(cond, true_r, false_r)?;
        b.set_region(true_r);
        b.build_return(None, &[])?;
        b.set_region(false_r);
        b.build_return(None, &[])?;
        b.build()?
    };

    // DeadBranchElimination alone should not fire because the condition
    // is a BoolBinaryOp node, not a BoolConst.
    assert!(!DeadBranchElimination.optimize(&mut fg.graph, fg.entry)?.changed());
    Ok(())
}

// ── Comprehensive tests ───────────────────────────────────────────────────────

/// `if(true)` nested inside the live branch of an outer `if(true)` — the
/// pipeline (ConstantFold + DBE + RedundantPhis) must eliminate both Ifs.
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
    let outer_cond = b.build_boolean_const(true);
    b.build_if(outer_cond, outer_t, outer_f)?;

    b.set_region(outer_t);
    let inner_cond = b.build_boolean_const(true);
    b.build_if(inner_cond, inner_t, inner_f)?;

    b.set_region(outer_f);
    b.build_return(None, &[])?;
    b.set_region(inner_t);
    let v = b.build_int_const(1u64, ir::ValueType::U64)?;
    b.build_return(Some(v), &[])?;
    b.set_region(inner_f);
    b.build_return(None, &[])?;

    let mut fg = b.build()?;

    let mut pipeline = OptimizerPipeline::new();
    pipeline.add(ConstantFold);
    pipeline.add(DeadBranchElimination);
    pipeline.add(RedundantPhis);
    pipeline.run(&mut fg.graph, fg.entry)?;

    let reachable: std::collections::HashSet<_> = fg.preorder().collect();
    let if_count = fg
        .all_node_ids()
        .filter(|n| reachable.contains(n))
        .filter(|&n| matches!(fg.graph.node_kind(n), NodeKind::If))
        .count();
    assert_eq!(if_count, 0, "both If nodes must be eliminated");
    Ok(())
}

/// Edge case: if the dead_ctrl output of an `If` is wired into the SAME
/// `ControlState` at *multiple* input slots, the previous code processed
/// `output_uses(dead_ctrl)` in arbitrary order and removed by the index
/// captured before mutation. After the first removal, indices shifted left,
/// so the second `remove_node_input` either:
///  - hit the `dead_idx < cs_len` guard and silently skipped (leaving a stale
///    dead reference in the ControlState), or
///  - was still in-bounds but pointed at the wrong (now live) predecessor and
///    removed it instead.
///
/// Sorting `dead_uses` by `(consumer, idx)` descending makes per-consumer
/// removals safe: removing the higher index first leaves all lower indices
/// pointing at their original slots. Different consumers don't interact.
///
/// Construction: build the standard `if(true)` skeleton, then wire `ctrl_false`
/// (the dead output) into the false-branch ControlState a second time via
/// `Graph::add_node_input`. `FunctionBuilder::build()` finishes before this
/// surgery, so its validator never sees the duplicate; we call
/// `DeadBranchElimination::optimize` directly (not the pipeline) for the same
/// reason.
#[test]
fn dead_branch_handles_dead_ctrl_wired_at_multiple_slots() -> Result<()> {
    let mut fg = make_if_fn(true)?;

    // Find the If node and its ctrl_false output (dead when cond=true).
    let if_node = fg
        .all_node_ids()
        .find(|&n| matches!(fg.graph.node_kind(n), NodeKind::If))
        .expect("expected an If node");
    let if_outputs: Vec<_> = fg.graph.node_outputs(if_node).into_iter().collect();
    assert_eq!(if_outputs.len(), 2, "If must have 2 control outputs");
    let ctrl_false = if_outputs[1];

    // Find the false-branch ControlState (the unique consumer of ctrl_false).
    let consumers: Vec<_> = fg.graph.output_uses(ctrl_false).collect();
    assert_eq!(
        consumers.len(),
        1,
        "ctrl_false should have exactly one consumer in the standard make_if_fn shape"
    );
    let false_cs = consumers[0].0;
    assert!(matches!(fg.graph.node_kind(false_cs), NodeKind::ControlState));

    // Wire ctrl_false into the same CS a second time, producing the bad shape.
    fg.graph.add_node_input(false_cs, ctrl_false)?;
    let pre_inputs: Vec<_> = fg.graph.node_inputs(false_cs).into_iter().collect();
    assert_eq!(pre_inputs.len(), 2);
    assert_eq!(
        pre_inputs[0], ctrl_false,
        "slot 0 must be ctrl_false (original)"
    );
    assert_eq!(
        pre_inputs[1], ctrl_false,
        "slot 1 must be ctrl_false (added duplicate)"
    );

    // Run DBE directly — pipeline.run() would re-validate, and we constructed
    // a deliberately-unusual (but IR-permitted) shape. DBE itself must handle
    // it without leaving a stale reference behind.
    DeadBranchElimination.optimize(&mut fg.graph, fg.entry)?;

    let post_inputs: Vec<_> = fg.graph.node_inputs(false_cs).into_iter().collect();
    assert_eq!(
        post_inputs.len(),
        0,
        "DBE must remove both dead-ctrl wires; got {} remaining input(s) {:?}",
        post_inputs.len(),
        post_inputs,
    );
    Ok(())
}

/// Regression: when an `If`'s dead control output is consumed *directly*
/// by a non-`ControlState` node (e.g. a `CallOther` that lost its
/// intermediate `ControlState` after `RedundantPhis` collapsed it), DBE
/// must not detach the `If`'s inputs and leave it as a 0-input zombie
/// reachable from the live graph via backward-data.
///
/// Shape under test:
///
///   entry ─┐
///          ▼
///         If(BoolConst(false))
///         ├── ctrl_true (dead) ──────► CallOther.ctrl_in   ◄── via surgery
///         └── ctrl_false (live) ─► CS_false ─► branch ─► join_CS ─► Return
///                                                 ▲
///                                  CallOther.ctrl_out (in true_r) wires here
///
/// Before the fix, DBE would:
///   1. replace `ctrl_false` with `ctrl_in` (live rewire),
///   2. skip the `CallOther` consumer of `ctrl_true` (Step 3 only handles
///      `ControlState`),
///   3. detach the `If`'s own inputs (Step 4),
///      leaving the `If` with 0 inputs.  The walker then re-reached the
///      `If` via `join_CS → CallOther → ctrl_true → If` (backward-data),
///      so the validator complained `node N has 0 inputs, expected 2`.
///
/// The fix drops Step 4 and instead returns `NoChange` when no real work
/// is left, keeping the `If`'s inputs intact and letting the dead-branch
/// subgraph stay as a structurally-valid zombie until the join's
/// `MemPhi` collapses through `RedundantPhis`.
#[test]
fn dead_branch_with_non_control_state_dead_consumer() -> Result<()> {
    let mut fg = {
        let mut b = FunctionBuilder::empty()?;
        let entry = b.create_region()?;
        let true_r = b.create_region()?;
        let false_r = b.create_region()?;
        let join = b.create_region()?;
        b.set_entry_region(entry)?;

        b.set_region(entry);
        let cond = b.build_boolean_const(false);
        b.build_if(cond, true_r, false_r)?;

        b.set_region(true_r);
        // v2's build_call_other_modeled does NOT advance the memory
        // token (caller decides via memory_edge).  The original test
        // exercised v1's conservative-clobber CallOther which DID
        // advance memory, so the join's MemPhi needs a non-trivial
        // mem-input shape.  Advance memory manually here.
        let (call_node, _, _) = b.build_call_other_modeled(0, "cpuid", &[], None, &[], &[], &[])?;
        let mem_out = b.body().graph.node_outputs(call_node)[1];
        b.advance_cur_region_memory(mem_out)?;
        b.build_branch(join)?;

        b.set_region(false_r);
        b.build_branch(join)?;

        b.set_region(join);
        b.build_return(None, &[])?;
        b.build()?
    };

    // Surgery: rewire the CallOther's ctrl input from CS_true.ctrl_out to
    // the If's dead_ctrl (= ctrl_true) directly, simulating the shape
    // RedundantPhis produces when it collapses an intermediate
    // single-predecessor ControlState.
    let if_node = fg
        .all_node_ids()
        .find(|&n| matches!(fg.graph.node_kind(n), NodeKind::If))
        .expect("If node");
    let if_outputs = fg.graph.node_outputs(if_node);
    let dead_ctrl = if_outputs[0]; // ctrl_true (cond=false → dead is true)

    let call_other = fg
        .all_node_ids()
        .find(|&n| matches!(fg.graph.node_kind(n), NodeKind::CallOther { .. }))
        .expect("CallOther node");

    let call_ctrl_input_id = fg.graph.node_input_id_at(call_other, 0)?;
    fg.graph.update_input(call_ctrl_input_id, dead_ctrl);

    // Run DBE in isolation, then validate.  Before the fix DBE detached
    // the If's inputs (Step 4) but left the non-CS dead consumer wired
    // to `dead_ctrl`, so the validator's reachability walk visited the
    // now-zero-input If via backward-data and reported
    // `NodeInputCountMismatch { expected: 2, actual: 0 }`.
    DeadBranchElimination.optimize(&mut fg.graph, fg.entry)?;
    ir::validate::validate(&fg.graph, fg.entry)
        .map_err(|e| anyhow::anyhow!("post-DBE validation failed: {e:?}"))?;

    // The If retains its [ctrl_in, cond] inputs; downstream cleanup
    // (MemPhi/VarPhi collapse + detach_unreachable) is responsible for
    // removing the dead-branch subgraph entirely, not DBE.
    let if_inputs = fg.graph.node_inputs(if_node);
    assert_eq!(
        if_inputs.len(),
        2,
        "If must retain its [ctrl_in, cond] inputs"
    );
    Ok(())
}

/// A VarPhi at a 2-input join — when the dead branch is removed, the
/// phi must lose exactly one input slot (the dead position).
#[test]
fn var_phi_loses_dead_slot() -> Result<()> {
    let var = reg_vn(0x1000, 8);
    let mut b = FunctionBuilder::new_raw(vec![var], &[var], &[], &[], None, 0)?;
    let entry = b.create_region()?;
    let true_r = b.create_region()?;
    let false_r = b.create_region()?;
    let join = b.create_region()?;
    b.set_entry_region(entry)?;

    b.set_region(entry);
    let cond = b.build_boolean_const(true);
    b.build_if(cond, true_r, false_r)?;

    b.set_region(true_r);
    let v_t = b.build_int_const(1u64, NodeOutputType::U64)?;
    b.write_variable(&var, v_t)?;
    b.build_branch(join)?;

    b.set_region(false_r);
    let v_f = b.build_int_const(2u64, NodeOutputType::U64)?;
    b.write_variable(&var, v_f)?;
    b.build_branch(join)?;

    b.set_region(join);
    let merged = b.read_variable(&var)?;
    b.build_return(Some(merged), &[])?;

    let mut fg = b.build()?;
    let pre_phi_count = fg
        .all_node_ids()
        .filter(|&n| matches!(fg.graph.node_kind(n), NodeKind::VarPhi(_)))
        .count();
    assert!(pre_phi_count > 0);

    DeadBranchElimination.optimize(&mut fg.graph, fg.entry)?;
    // A VarPhi at the join should now have only the live predecessor's
    // value input (length = 1 token + 1 value = 2).
    let join_phi = fg
        .all_node_ids()
        .find(|&n| matches!(fg.graph.node_kind(n), NodeKind::VarPhi(v) if *v == var))
        .expect("control phi at join must exist");
    let phi_inputs = fg.graph.node_inputs(join_phi);
    assert_eq!(phi_inputs.len(), 2, "phi must have exactly 1 live value");
    Ok(())
}
