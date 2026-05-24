use super::*;
use crate::opt::pipeline::Optimizer;
use crate::opt::{ConstantFold, OptimizerPipeline};
use strider_ir::node::{NodeKind, NodeOutputType};
use strider_ir_test_utils::{sp_vn_x86 as sp_vn, RegisterSet, SENTINEL_LIFT_ADDR};
use strider_ir::{FunctionBuilder, IntBinaryOp};

// ── Original test ─────────────────────────────────────────────────────────────

/// Two reachable CFG predecessors feed the same `NodeOutputId` into the
/// VarPhi at the join — exactly the shape the analyzer produces for
/// SP across an `if/else` where both arms write the same pre-computed
/// value (e.g. the loop-prologue `sub esp, 4` shared by the loop-entry
/// and loop-continue edges).  Without the "all live values identical"
/// rule the phi survives (distinct ctrl predecessors), leaving a
/// spurious `φ ESP` in the output.
#[test]
fn phi_with_identical_data_inputs_is_removed() -> crate::opt::Result<()> {
    let sp = sp_vn();
    let mut b = RegisterSet::new().tracked(sp).callee_saved(sp).build_fn()?;
    let entry = b.create_region()?;
    let a = b.create_region()?;
    let bb = b.create_region()?;
    let c = b.create_region()?;
    b.set_entry_region(entry)?;

    // entry: shared = sp - 4; if cond goto a else goto b
    b.set_region(entry);
    let sp_entry = b.read_variable(&sp)?;
    let four = b.build_int_const(4u64, NodeOutputType::U32)?;
    let shared_sp =
        b.build_int_sub(sp_entry, four, NodeOutputType::U32)?;
    let cond = b.build_boolean_const(true);
    b.build_if(cond, a, bb)?;

    // a: sp = shared
    b.set_region(a);
    b.write_variable(&sp, shared_sp)?;
    b.build_branch(c)?;

    // b: sp = shared  (same NodeOutputId, so phi at c will have both
    // data inputs equal).
    b.set_region(bb);
    b.write_variable(&sp, shared_sp)?;
    b.build_branch(c)?;

    // c: load through sp so the phi's output has a live use.
    b.set_region(c);
    let sp_c = b.read_variable(&sp)?;
    let loaded = b.build_load(sp_c, rsleigh::VnSpace::RAM, NodeOutputType::U32)?;
    b.build_return(Some(loaded), &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    let mut pipeline = OptimizerPipeline::new();
    pipeline.add(ConstantFold);
    pipeline.add(RedundantPhis);
    pipeline.run_built(fg.graph_mut())?;

    // The only VarPhi(sp) at `c` had both predecessors feeding the
    // same Sub output — must be gone after the pass.
    let reachable: entity_utils::DenseEntitySet<strider_ir::node::NodeId> =
        fg.preorder().collect();
    let surviving_sp_phis = fg
        .all_node_ids()
        .filter(|&n| reachable.contains(n)
            && matches!(fg.node_kind(n), NodeKind::Phi)
            && fg.phi_var_tag(n) == Some(sp))
        .count();
    assert_eq!(
        surviving_sp_phis, 0,
        "VarPhi(sp) with identical data inputs must be removed"
    );
    Ok(())
}

// ── Comprehensive tests ───────────────────────────────────────────────────────

/// MemPhi with a single reachable predecessor must be eliminated.
/// A simple Store creates a memory chain through the body region; with one
/// CFG predecessor from entry, the MemPhi at the body's join has only one
/// live input and must collapse.
#[test]
fn mem_phi_single_pred_eliminated() -> crate::opt::Result<()> {
    let mut b = FunctionBuilder::empty()?;
    let entry = b.create_region()?;
    let body = b.create_region()?;
    b.set_entry_region(entry)?;
    b.set_region(entry);
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
    b.build_branch(body)?;
    b.set_region(body);
    let addr = b.build_int_const(0x1000u64, NodeOutputType::U64)?;
    let data = b.build_int_const(0x42u64, NodeOutputType::U64)?;
    b.build_store(addr, data, rsleigh::VnSpace::RAM)?;
    b.build_return(None, &[])?;
    b.set_lift_addr(None);

    let mut fg = b.build()?;
    let entry = fg.entry().unwrap();
    RedundantPhis.optimize(fg.graph_mut(), entry)?;

    // Surviving (reachable) MemPhis with at most 1+1 inputs (token + 1 value)
    // must be 0.  `count_reachable` only filters by `NodeKind`, so the
    // arity-additional check is inline below — `preorder()` is the
    // reachable iterator the helper itself uses internally.
    let surviving = fg
        .preorder()
        .filter(|&n| matches!(fg.node_kind(n), NodeKind::MemPhi))
        .filter(|&n| fg.node_inputs(n).len() <= 2)
        .count();
    assert_eq!(surviving, 0);
    Ok(())
}

/// Two-region function (entry → body → return). The body's ControlState has
/// one reachable predecessor; RedundantPhis must report `Changed` and either
/// detach the body CS or bypass its ctrl edge so the Return reads from the
/// entry's ctrl output directly.
#[test]
fn control_state_single_pred_collapses() -> crate::opt::Result<()> {
    let mut b = FunctionBuilder::empty()?;
    let entry = b.create_region()?;
    let body = b.create_region()?;
    b.set_entry_region(entry)?;
    b.set_region(entry);
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
    b.build_branch(body)?;
    b.set_region(body);
    b.build_return(None, &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;
    let entry = fg.entry().unwrap();
    assert!(
        RedundantPhis.optimize(fg.graph_mut(), entry)?.changed(),
        "single-pred CS must be simplified"
    );
    Ok(())
}

/// Validate-at-end: an `if(false)` branch's store ends up unreachable;
/// the pipeline (ConstantFold + DeadBranchElimination + RedundantPhis)
/// must leave a graph that passes IR validation.
#[test]
fn unreachable_store_inputs_detached() -> crate::opt::Result<()> {
    let mut b = FunctionBuilder::empty()?;
    let entry = b.create_region()?;
    let dead = b.create_region()?;
    let live = b.create_region()?;
    b.set_entry_region(entry)?;
    b.set_region(entry);
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
    let cond = b.build_boolean_const(false);
    b.build_if(cond, dead, live)?;
    b.set_region(dead);
    let addr_d = b.build_int_const(0xDEADu64, NodeOutputType::U64)?;
    let data_d = b.build_int_const(0xBADCu64, NodeOutputType::U64)?;
    b.build_store(addr_d, data_d, rsleigh::VnSpace::RAM)?;
    b.build_return(None, &[])?;
    b.set_region(live);
    b.build_return(None, &[])?;
    b.set_lift_addr(None);

    let mut fg = b.build()?;
    let mut pipeline = OptimizerPipeline::new();
    pipeline.add(crate::opt::ConstantFold);
    pipeline.add(crate::opt::DeadBranchElimination);
    pipeline.add(RedundantPhis);
    pipeline.run_built(fg.graph_mut())?;
    // Validation runs at the end of `pipeline.run`, so reaching here means
    // the unreachable store didn't leave an invalid graph.
    Ok(())
}

/// RedundantPhis must not report `Changed` when its only effect is detaching
/// the inputs of CFG-unreachable zombies.  Detached zombies cannot be
/// consumed by reachable nodes, so no other pass can act on the result —
/// escalating the cleanup to `Changed` just costs the pipeline one extra
/// fixed-point iteration.
///
/// Test fixture: run the default pipeline once to fully settle any real
/// phi/state simplification, then graft an orphan Add node directly onto
/// the arena via `Graph::create_node` — unreachable from `entry`, with
/// non-empty inputs.  On the next RedundantPhis invocation the only thing
/// the pass can do is detach that orphan's inputs, so the result must be
/// `NoChange`.
#[test]
fn redundant_phis_no_changed_for_orphan_only_cleanup() -> crate::opt::Result<()> {
    use strider_ir::node::NodeOutputKind;
    let mut b = FunctionBuilder::empty()?;
    let entry = b.create_region()?;
    b.set_entry_region(entry)?;
    b.set_region(entry);
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
    let c = b.build_int_const(0u64, NodeOutputType::U64)?;
    b.build_return(Some(c), &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    // Settle the graph by running RedundantPhis to fixed point first.  After
    // this, any further RedundantPhis invocation on the unmodified graph
    // returns NoChange; that's our baseline.
    let entry = fg.entry().unwrap();
    while RedundantPhis.optimize(fg.graph_mut(), entry)?.changed() {}
    let entry = fg.entry().unwrap();
    let baseline = RedundantPhis.optimize(fg.graph_mut(), entry)?;
    assert_eq!(
        baseline,
        OptimizationResult::NoChange,
        "graph should be settled before grafting the orphan"
    );

    // Now graft an unreachable Add whose inputs are real outputs of
    // reachable nodes. The Add itself is not consumed by anything reachable,
    // so `preorder()` will not include it; `detach_unreachable_nodes` is the
    // only thing in RedundantPhis that can touch it.
    let one = fg.make_int_const(1u64, NodeOutputType::U64)?;
    let two = fg.make_int_const(2u64, NodeOutputType::U64)?;
    let _orphan = fg.create_node(
        NodeKind::IntBinaryOp(IntBinaryOp::Add),
        [one, two],
        [NodeOutputKind::OutputType(NodeOutputType::U64)],
    );

    let entry = fg.entry().unwrap();
    let res = RedundantPhis.optimize(fg.graph_mut(), entry)?;
    assert_eq!(
        res,
        OptimizationResult::NoChange,
        "RedundantPhis must report NoChange when its only effect is orphan-input detachment"
    );
    Ok(())
}

/// Transient mid-opt arity violation: a peer pass running in the same
/// fixed-point loop can momentarily leave a `Phi` whose value-input
/// arity does not match its owning `ControlState`'s ctrl-edge count.
/// `RedundantPhis` must surface this as a typed error (`Err`) rather than
/// panicking on slice indexing — the fixed-point loop will rerun and
/// the next iteration sees the repaired arity.
#[test]
fn transient_arity_mismatch_surfaces_as_error_not_panic() -> crate::opt::Result<()> {
    use strider_ir_test_utils::reg_vn;
    let var = reg_vn(0x1000, 8);
    let mut b = RegisterSet::new().tracked(var).arg(var).build_fn()?;
    let entry = b.create_region()?;
    let join = b.create_region()?;
    b.set_entry_region(entry)?;

    // entry: branch to join.
    b.set_region(entry);
    b.build_branch(join)?;

    // join: read `var` (creates the VarPhi(var) we'll deform), then return.
    b.set_region(join);
    let read_back = b.read_variable(&var)?;
    b.build_return(Some(read_back), &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    // Locate the VarPhi(var) and its owning CS.
    let phi_node = fg
        .all_node_ids()
        .find(|&n| matches!(fg.node_kind(n), NodeKind::Phi) && fg.phi_var_tag(n) == Some(var))
        .expect("VarPhi(var) at join");
    let phi_token = fg.node_inputs(phi_node)[0];
    let cs_node = fg.output_definition(phi_token).0;

    // Surgery: append a new ctrl input to the CS WITHOUT appending the
    // matching value to the VarPhi.  This simulates a peer pass that
    // attached a new predecessor's ctrl edge but had not yet wired
    // value/MemPhi inputs.
    let cs_ctrl_out = fg.node_outputs(cs_node)[0];
    fg.add_node_input(cs_node, cs_ctrl_out)?;

    // Sanity: the CS now has 2 ctrl inputs, but the phi only has token + 1
    // value (2 inputs total), so accessing `inputs[2]` would panic.
    assert_eq!(
        fg.node_inputs(cs_node).len(),
        2,
        "CS has 2 ctrl edges after surgery"
    );
    assert_eq!(
        fg.node_inputs(phi_node).len(),
        2,
        "VarPhi has only token + 1 value after deliberate surgery"
    );

    let entry = fg.entry().unwrap();
    let result = RedundantPhis.optimize(fg.graph_mut(), entry);
    let err = result.expect_err(
        "RedundantPhis must surface transient phi-arity violation as a typed Err, not panic"
    );
    let msg = format!("{err:#}");
    assert!(
        msg.contains("transient mid-opt"),
        "error must identify the failure mode: got {msg:?}"
    );
    Ok(())
}

/// Loop-style self-referential `VarPhi`: one operand is the entry value, the
/// other is the phi's own output (the back-edge of an unsimplified loop where
/// the variable is never modified inside the body).  Braun-style trivial-phi
/// detection must skip the self-reference and collapse the phi to the entry
/// value.
///
/// **Surfaces in real lifts:** `getnanouptime`'s seqlock retry loop reads
/// the function-arg pointer at the loop header and never mutates it inside
/// the body; the IR's loop-header `VarPhi` has [arg0_from_entry,
/// phi_self_from_loop_back] as its two value inputs.  Without this
/// reduction, `FunctionArgDetect` can't recognise the value as the
/// canonical `FunctionArg(0)`.
#[test]
fn phi_with_self_referential_back_edge_collapses() -> crate::opt::Result<()> {
    use strider_ir_test_utils::reg_vn;
    let var = reg_vn(0x1000, 8);
    let mut b = RegisterSet::new().tracked(var).arg(var).build_fn()?;
    let entry = b.create_region()?;
    let join = b.create_region()?;
    b.set_entry_region(entry)?;

    // entry: branch to join.
    b.set_region(entry);
    b.build_branch(join)?;

    // join: read `var` (creates a single-input VarPhi at this region's
    // CS), then return.
    b.set_region(join);
    let read_back = b.read_variable(&var)?;
    b.build_return(Some(read_back), &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    // Locate the VarPhi(var) at `join` and its owning CS.
    let phi_node = fg
        .all_node_ids()
        .find(|&n| matches!(fg.node_kind(n), NodeKind::Phi)
            && fg.phi_var_tag(n) == Some(var))
        .expect("VarPhi(var) at join");
    let phi_inputs_pre = fg.node_inputs(phi_node);
    let phi_token = phi_inputs_pre[0];
    let initial_value = phi_inputs_pre[1];
    let cs_node = fg.output_definition(phi_token).0;

    // Surgery: append a second, *distinct* control predecessor to the
    // join CS — the join's own ctrl_out, modelling a direct self-loop
    // back-edge (the simplest loop shape that gets us two distinct
    // reachable predecessors so the existing
    // "all-data-inputs-identical" rule cannot fire by collapsing the
    // ctrl-set first).  Then append a matching second value to *every*
    // phi owned by that CS — the VarPhi gets its own output (the
    // loop-back self-ref the test exercises), and any MemPhi gets
    // *its* own output (so the graph keeps the per-predecessor arity
    // invariant `remove_phis` relies on).
    let cs_outputs = fg.node_outputs(cs_node);
    let cs_ctrl_out = cs_outputs[0];
    let cs_phi_out = cs_outputs[1];
    fg.add_node_input(cs_node, cs_ctrl_out)?;

    let phi_consumers: Vec<strider_ir::node::NodeId> = fg
        .output_uses(cs_phi_out)
        .map(|(n, _)| n)
        .collect();
    for phi in phi_consumers {
        let self_out = fg.node_outputs_exact::<1>(phi)?[0];
        fg.add_node_input(phi, self_out)?;
    }
    assert_eq!(
        fg.node_inputs(phi_node).len(),
        3,
        "VarPhi must have [token, initial, self-ref] = 3 inputs after surgery"
    );

    // Run the pass under test.
    let entry = fg.entry().unwrap();
    RedundantPhis.optimize(fg.graph_mut(), entry)?;

    // After collapse, the Return's value input must reference the
    // initial entry value, *not* the phi's output.  `replace_all_uses`
    // on the phi's output rewires the Return to consume `initial_value`
    // directly.
    let return_node = fg
        .all_node_ids()
        .find(|&n| matches!(fg.node_kind(n), NodeKind::Return))
        .expect("Return");
    let ret_val = fg.node_inputs(return_node)[2];
    assert_eq!(
        ret_val, initial_value,
        "Return's value must be rewired to the phi's only non-self-referential operand"
    );
    Ok(())
}
