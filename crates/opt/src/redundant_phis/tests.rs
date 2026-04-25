use super::*;
use crate::{ConstantFold, OptimizerPipeline};
use ir::node::{NodeKind, NodeOutputType};
use ir::{FunctionBuilder, IntBinaryOp};

fn sp_vn() -> rsleigh::Vn {
    rsleigh::Vn {
        addr: rsleigh::VnAddr {
            space: rsleigh::VnSpace::REGISTER,
            off: 0x20,
        },
        size: 4,
    }
}

// ── Original test ─────────────────────────────────────────────────────────────

/// Two reachable CFG predecessors feed the same `NodeOutputId` into the
/// ControlPhi at the join — exactly the shape the analyzer produces for
/// SP across an `if/else` where both arms write the same pre-computed
/// value (e.g. the loop-prologue `sub esp, 4` shared by the loop-entry
/// and loop-continue edges).  Without the "all live values identical"
/// rule the phi survives (distinct ctrl predecessors), leaving a
/// spurious `φ ESP` in the output.
#[test]
fn phi_with_identical_data_inputs_is_removed() -> crate::Result<()> {
    let sp = sp_vn();
    let mut b = FunctionBuilder::new_raw(vec![sp], &[], &[sp], &[], None, 0)?;
    let entry = b.create_region()?;
    let a = b.create_region()?;
    let bb = b.create_region()?;
    let c = b.create_region()?;
    b.set_entry_region(entry)?;

    // entry: shared = sp - 4; if cond goto a else goto b
    b.set_region(entry);
    let sp_entry = b.read_variable(&sp)?;
    let four = b.build_int_const(4, NodeOutputType::U32).unwrap();
    let shared_sp =
        b.build_int_binary_operation(sp_entry, four, IntBinaryOp::Sub, NodeOutputType::U32)?;
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
    let mut fg = b.build()?;

    let mut pipeline = OptimizerPipeline::new();
    pipeline.add(ConstantFold);
    pipeline.add(RedundantPhis);
    pipeline.run(&mut fg)?;

    // The only ControlPhi(sp) at `c` had both predecessors feeding the
    // same Sub output — must be gone after the pass.
    let reachable: std::collections::HashSet<_> = fg.preorder().collect();
    let surviving_sp_phis = fg
        .all_node_ids()
        .filter(|n| reachable.contains(n))
        .filter(|&n| matches!(fg.graph.node_kind(n), NodeKind::ControlPhi(vn) if *vn == sp))
        .count();
    assert_eq!(
        surviving_sp_phis, 0,
        "ControlPhi(sp) with identical data inputs must be removed"
    );
    Ok(())
}

// ── Comprehensive tests ───────────────────────────────────────────────────────

/// MemPhi with a single reachable predecessor must be eliminated.
/// A simple Store creates a memory chain through the body region; with one
/// CFG predecessor from entry, the MemPhi at the body's join has only one
/// live input and must collapse.
#[test]
fn mem_phi_single_pred_eliminated() -> crate::Result<()> {
    let mut b = FunctionBuilder::new_raw(vec![], &[], &[], &[], None, 0)?;
    let entry = b.create_region()?;
    let body = b.create_region()?;
    b.set_entry_region(entry)?;
    b.set_region(entry);
    b.build_branch(body)?;
    b.set_region(body);
    let addr = b.build_int_const(0x1000, NodeOutputType::U64).unwrap();
    let data = b.build_int_const(0x42, NodeOutputType::U64).unwrap();
    b.build_store(addr, data, rsleigh::VnSpace::RAM)?;
    b.build_return(None, &[])?;

    let mut fg = b.build()?;
    RedundantPhis.optimize(&mut fg)?;

    // Surviving (reachable) MemPhis with at most 1+1 inputs (token + 1 value)
    // must be 0.
    let reachable: std::collections::HashSet<_> = fg.preorder().collect();
    let surviving = fg
        .all_node_ids()
        .filter(|n| reachable.contains(n))
        .filter(|&n| matches!(fg.graph.node_kind(n), NodeKind::MemPhi))
        .filter(|&n| fg.graph.node_inputs(n).len() <= 2)
        .count();
    assert_eq!(surviving, 0);
    Ok(())
}

/// Two-region function (entry → body → return). The body's ControlState has
/// one reachable predecessor; RedundantPhis must report `Changed` and either
/// detach the body CS or bypass its ctrl edge so the Return reads from the
/// entry's ctrl output directly.
#[test]
fn control_state_single_pred_collapses() -> crate::Result<()> {
    let mut b = FunctionBuilder::new_raw(vec![], &[], &[], &[], None, 0)?;
    let entry = b.create_region()?;
    let body = b.create_region()?;
    b.set_entry_region(entry)?;
    b.set_region(entry);
    b.build_branch(body)?;
    b.set_region(body);
    b.build_return(None, &[])?;
    let mut fg = b.build()?;
    assert!(
        RedundantPhis.optimize(&mut fg)?.changed(),
        "single-pred CS must be simplified"
    );
    Ok(())
}

/// Validate-at-end: an `if(false)` branch's store ends up unreachable;
/// the pipeline (ConstantFold + DeadBranchElimination + RedundantPhis)
/// must leave a graph that passes IR validation.
#[test]
fn unreachable_store_inputs_detached() -> crate::Result<()> {
    let mut b = FunctionBuilder::new_raw(vec![], &[], &[], &[], None, 0)?;
    let entry = b.create_region()?;
    let dead = b.create_region()?;
    let live = b.create_region()?;
    b.set_entry_region(entry)?;
    b.set_region(entry);
    let cond = b.build_boolean_const(false);
    b.build_if(cond, dead, live)?;
    b.set_region(dead);
    let addr_d = b.build_int_const(0xDEAD, NodeOutputType::U64).unwrap();
    let data_d = b.build_int_const(0xBADC, NodeOutputType::U64).unwrap();
    b.build_store(addr_d, data_d, rsleigh::VnSpace::RAM)?;
    b.build_return(None, &[])?;
    b.set_region(live);
    b.build_return(None, &[])?;

    let mut fg = b.build()?;
    let mut pipeline = OptimizerPipeline::new();
    pipeline.add(crate::ConstantFold);
    pipeline.add(crate::DeadBranchElimination);
    pipeline.add(RedundantPhis);
    pipeline.run(&mut fg)?;
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
fn redundant_phis_no_changed_for_orphan_only_cleanup() -> crate::Result<()> {
    use ir::node::NodeOutputKind;
    let mut b = FunctionBuilder::new_raw(vec![], &[], &[], &[], None, 0)?;
    let entry = b.create_region()?;
    b.set_entry_region(entry)?;
    b.set_region(entry);
    let c = b.build_int_const(0, NodeOutputType::U64);
    b.build_return(Some(c), &[])?;
    let mut fg = b.build()?;

    // Settle the graph by running RedundantPhis to fixed point first.  After
    // this, any further RedundantPhis invocation on the unmodified graph
    // returns NoChange; that's our baseline.
    while RedundantPhis.optimize(&mut fg)?.changed() {}
    let baseline = RedundantPhis.optimize(&mut fg)?;
    assert_eq!(
        baseline,
        OptimizationResult::NoChange,
        "graph should be settled before grafting the orphan"
    );

    // Now graft an unreachable Add whose inputs are real outputs of
    // reachable nodes. The Add itself is not consumed by anything reachable,
    // so `preorder()` will not include it; `detach_unreachable_nodes` is the
    // only thing in RedundantPhis that can touch it.
    let one = fg.make_int_const(1, NodeOutputType::U64)?;
    let two = fg.make_int_const(2, NodeOutputType::U64)?;
    let _orphan = fg.graph.create_node(
        NodeKind::IntBinaryOp(IntBinaryOp::Add),
        [one, two],
        [NodeOutputKind::OutputType(NodeOutputType::U64)],
    );

    let res = RedundantPhis.optimize(&mut fg)?;
    assert_eq!(
        res,
        OptimizationResult::NoChange,
        "RedundantPhis must report NoChange when its only effect is orphan-input detachment"
    );
    Ok(())
}
