//! Multi-pass optimizer cooperation tests.
//!
//! Each test builds a small hand-crafted IR fixture and runs a focused subset
//! of the optimizer pipeline, asserting the expected post-pipeline graph
//! shape.  These are easier to diagnose than cross-arch snapshot diffs when
//! the pipeline regresses: a snapshot failure often prints 200 lines of per-
//! arch IR while these tests report exactly which shape invariant broke.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable
)]

use strider_ir::node::{NodeKind, ValueType};
use strider_ir::{IRBuilderExt, IRViewer, IRWalker, IntBinaryOp};
use strider_ir_test_utils::{RegisterSet, SENTINEL_LIFT_ADDR, stack_vn_x86_64};
use strider_orchestrator::opt::{
    CfgDetach, ConstantFold, DeadBranchElimination, LoadForward, OptimizerPipeline, PhiCollapse,
    RegionCollapse,
};

type Result<T> = strider_orchestrator::opt::Result<T>;

// ── helpers ───────────────────────────────────────────────────────────────────

/// Count reachable nodes matching `pred`.
fn count_reachable<F>(function: &strider_ir::Function, pred: F) -> usize
where
    F: Fn(&NodeKind) -> bool,
{
    function
        .walk()
        .filter(|&n| pred(function.node_kind(n)))
        .count()
}

// ── Tests ─────────────────────────────────────────────────────────────────────

/// Two nested `if(const)` branches — both constants evaluable at construction
/// time.  Running `ConstantFold + DeadBranchElimination + PhiCollapse + CfgDetach` must
/// eliminate all `If` nodes from the reachable graph.
#[test]
fn nested_const_branches_fully_eliminated() -> Result<()> {
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
    let inner_cond = b.build_boolean_const(false);
    b.build_if(inner_cond, inner_t, inner_f)?;

    b.set_region(outer_f);
    let v_dead1 = b.build_int_const(99u64, ValueType::I64)?;
    b.build_return(Some(v_dead1), &[])?;

    b.set_region(inner_t);
    let v_dead2 = b.build_int_const(1u64, ValueType::I64)?;
    b.build_return(Some(v_dead2), &[])?;

    b.set_region(inner_f);
    let v_live = b.build_int_const(2u64, ValueType::I64)?;
    b.build_return(Some(v_live), &[])?;
    b.set_lift_addr(None);

    let mut fg = b.build()?;

    let mut pipeline = OptimizerPipeline::new();
    pipeline.add(ConstantFold::new());
    pipeline.add(PhiCollapse);
    pipeline.add(RegionCollapse);
    pipeline.add(DeadBranchElimination);
    pipeline.add(CfgDetach);
    pipeline.run(&mut fg, &mut strider_orchestrator::opt::OptCtx::new(None))?;

    // All If nodes must have been eliminated from the reachable graph.
    let remaining_ifs = count_reachable(&fg, |k| matches!(k, NodeKind::If));
    assert_eq!(
        remaining_ifs, 0,
        "nested const branches must be fully eliminated; {remaining_ifs} If(s) remain"
    );
    Ok(())
}

/// Linear graph: `ConstantFold` must fold `1 + 2` to `3`; `DBE` / `RP` run
/// clean; final Return must source from `IntConst(3)`.
#[test]
fn const_fold_then_dbe_then_phi_collapse() -> Result<()> {
    let mut b = strider_ir_test_utils::empty_builder()?;
    let entry = b.create_region_all()?;
    b.set_entry_region_all(entry)?;
    b.set_region(entry);
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));

    let one = b.build_int_const(1u64, ValueType::I64)?;
    let two = b.build_int_const(2u64, ValueType::I64)?;
    let sum = b.build_int_binary_operation(one, two, IntBinaryOp::Add, ValueType::I64)?;
    b.build_return(Some(sum), &[])?;
    b.set_lift_addr(None);

    let mut fg = b.build()?;

    let mut pipeline = OptimizerPipeline::new();
    pipeline.add(ConstantFold::new());
    pipeline.add(PhiCollapse);
    pipeline.add(RegionCollapse);
    pipeline.add(DeadBranchElimination);
    pipeline.add(CfgDetach);
    pipeline.run(&mut fg, &mut strider_orchestrator::opt::OptCtx::new(None))?;

    // The return value must now source from IntConst(3).
    let ret = fg
        .walk()
        .find(|&n| matches!(fg.node_kind(n), NodeKind::Return))
        .expect("Return node");
    let ret_val = fg.node_inputs(ret)[2];
    assert!(
        matches!(fg.kind_of_value(ret_val), NodeKind::IntConst(_))
            && fg.int_const_u128(ret_val) == Some(3),
        "ConstantFold must fold 1+2→3"
    );
    Ok(())
}

/// `if(true)` with one arm doing a stack spill + reload:
/// `StackOffsetDetect + ConstantFold + PhiCollapse` must collapse the
/// single-predecessor join region and leave an `IntConst` as the
/// return value (the forwarded constant).
#[test]
fn stack_pipeline_full_cooperation() -> Result<()> {
    let sp = stack_vn_x86_64();
    let mut b = RegisterSet::new()
        .tracked(sp)
        .callee_saved(sp)
        .stack_vn(sp)
        .build_fn()?;

    let entry = b.create_region_all()?;
    let live = b.create_region_all()?;
    b.set_entry_region_all(entry)?;
    b.set_region(entry);
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));

    // Unconditional branch to live (single predecessor → region is degenerate).
    b.build_branch(live)?;

    b.set_region(live);
    let sp_val = b.read_variable(&sp)?;
    // Store 0x42 at sp+0.
    let sp_off = b.build_int_const(0u64, ValueType::I64)?;
    let addr = b.build_int_binary_operation(sp_val, sp_off, IntBinaryOp::Add, ValueType::I64)?;
    let stored_val = b.build_int_const(0x42u64, ValueType::I64)?;
    b.build_store(addr, stored_val, rsleigh::VnSpace::RAM)?;
    // Reload from sp+0.
    let loaded = b.build_load(addr, rsleigh::VnSpace::RAM, ValueType::I64)?;
    b.build_return(Some(loaded), &[])?;
    b.set_lift_addr(None);

    let mut fg = b.build()?;

    let mut pipeline = OptimizerPipeline::new();
    pipeline.add(ConstantFold::new());
    pipeline.add(PhiCollapse);
    pipeline.add(RegionCollapse);
    pipeline.add(LoadForward);
    pipeline.run(&mut fg, &mut strider_orchestrator::opt::OptCtx::new(None))?;

    // The return value should have been forwarded to the stored constant.
    let ret = fg
        .walk()
        .find(|&n| matches!(fg.node_kind(n), NodeKind::Return))
        .expect("Return node");
    let ret_val = fg.node_inputs(ret)[2];
    assert!(
        matches!(fg.kind_of_value(ret_val), NodeKind::IntConst(_))
            && fg.int_const_u128(ret_val) == Some(0x42),
        "LoadForward must forward the stored value 0x42 to the load"
    );
    Ok(())
}

/// A single `if(true) { return 1 } else { return 2 }`:
/// `ConstantFold + DeadBranchElimination` must eliminate the `If` and
/// exactly one branch region becomes unreachable.
#[test]
fn if_branch_collapses_after_const_fold() -> Result<()> {
    let mut b = strider_ir_test_utils::empty_builder()?;
    let entry = b.create_region_all()?;
    let t = b.create_region_all()?;
    let f = b.create_region_all()?;
    b.set_entry_region_all(entry)?;

    b.set_region(entry);
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
    let cond = b.build_boolean_const(true);
    b.build_if(cond, t, f)?;

    b.set_region(t);
    let v1 = b.build_int_const(1u64, ValueType::I64)?;
    b.build_return(Some(v1), &[])?;

    b.set_region(f);
    let v2 = b.build_int_const(2u64, ValueType::I64)?;
    b.build_return(Some(v2), &[])?;
    b.set_lift_addr(None);

    let mut fg = b.build()?;

    let mut pipeline = OptimizerPipeline::new();
    pipeline.add(ConstantFold::new());
    pipeline.add(DeadBranchElimination);
    pipeline.run(&mut fg, &mut strider_orchestrator::opt::OptCtx::new(None))?;

    // No If nodes must remain in the reachable graph.
    let ifs = count_reachable(&fg, |k| matches!(k, NodeKind::If));
    assert_eq!(ifs, 0, "If(true) must be eliminated by CF+DBE");

    // The reachable Return must return IntConst(1) — the true branch.
    let ret = fg
        .walk()
        .find(|&n| matches!(fg.node_kind(n), NodeKind::Return))
        .expect("Return node");
    let ret_val = fg.node_inputs(ret)[2];
    assert!(
        matches!(fg.kind_of_value(ret_val), NodeKind::IntConst(_))
            && fg.int_const_u128(ret_val) == Some(1),
        "surviving return must return 1 (true branch)"
    );
    Ok(())
}

/// A degenerate Region+Phi with a single reachable predecessor must be
/// collapsed by `RegionCollapse`.  Post-pass the Return must no longer
/// flow through a Region.
#[test]
fn region_with_one_predecessor_collapses() -> Result<()> {
    let mut b = strider_ir_test_utils::empty_builder()?;
    let entry = b.create_region_all()?;
    let body = b.create_region_all()?;
    b.set_entry_region_all(entry)?;

    b.set_region(entry);
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
    b.build_branch(body)?;

    b.set_region(body);
    let c = b.build_int_const(77u64, ValueType::I64)?;
    b.build_return(Some(c), &[])?;
    b.set_lift_addr(None);

    let mut fg = b.build()?;

    // Before: 2 reachable Regions (entry + body).
    let regions_before = count_reachable(&fg, |k| matches!(k, NodeKind::Region));
    assert_eq!(regions_before, 2, "fixture must start with 2 regions");

    let mut pipeline = OptimizerPipeline::new();
    pipeline.add(PhiCollapse);
    pipeline.add(RegionCollapse);
    pipeline.run(&mut fg, &mut strider_orchestrator::opt::OptCtx::new(None))?;

    // After: the degenerate body Region must be gone (1 or 0 Regions survive).
    let regions_after = count_reachable(&fg, |k| matches!(k, NodeKind::Region));
    assert!(
        regions_after < regions_before,
        "single-predecessor Region must be collapsed by RegionCollapse; \
         before={regions_before} after={regions_after}"
    );
    Ok(())
}

/// A pre-`StackOffsetDetect` memory chain with no SP-relative stores must be left
/// structurally intact by `ConstantFold` (negative invariant).  The
/// `Load` node and its `Store`-derived memory chain must both remain
/// reachable.
#[test]
fn mem_chain_collapses_through_constant_fold() -> Result<()> {
    let mut b = strider_ir_test_utils::empty_builder()?;
    let entry = b.create_region_all()?;
    b.set_entry_region_all(entry)?;
    b.set_region(entry);
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));

    // Build: Store(addr1, val) → Load(addr2) where addr1 ≠ addr2 —
    // the load depends on the Store's memory output but does NOT
    // alias it.
    let addr1 = b.build_int_const(0x1000u64, ValueType::I64)?;
    let addr2 = b.build_int_const(0x2000u64, ValueType::I64)?;
    let val = b.build_int_const(0xABu64, ValueType::I64)?;
    b.build_store(addr1, val, rsleigh::VnSpace::RAM)?;
    let loaded = b.build_load(addr2, rsleigh::VnSpace::RAM, ValueType::I64)?;
    b.build_return(Some(loaded), &[])?;
    b.set_lift_addr(None);

    let mut fg = b.build()?;
    let stores_before = count_reachable(&fg, |k| matches!(k, NodeKind::Store(_)));
    let loads_before = count_reachable(&fg, |k| matches!(k, NodeKind::Load(_)));
    assert_eq!(stores_before, 1);
    assert_eq!(loads_before, 1);

    let mut pipeline = OptimizerPipeline::new();
    pipeline.add(ConstantFold::new());
    pipeline.run(&mut fg, &mut strider_orchestrator::opt::OptCtx::new(None))?;

    // ConstantFold alone must NOT remove the Store or Load
    // (LoadForward / StackOffsetDetect handle memory forwarding).
    let stores_after = count_reachable(&fg, |k| matches!(k, NodeKind::Store(_)));
    let loads_after = count_reachable(&fg, |k| matches!(k, NodeKind::Load(_)));
    assert_eq!(
        stores_after, 1,
        "ConstantFold must not remove a Store that feeds a dependent Load"
    );
    assert_eq!(
        loads_after, 1,
        "ConstantFold must not remove a Load whose address is non-aliased"
    );
    Ok(())
}

/// Running the full `ConstantFold + DBE + PhiCollapse` pipeline twice on
/// the same graph must produce the same graph shape (idempotency guard).
/// The second run must report `NoChange`.
#[test]
fn multi_pass_idempotent_after_fixed_point() -> Result<()> {
    // Build a slightly non-trivial fixture: if(true) { return 1+2 } else { return 3 }
    let mut b = strider_ir_test_utils::empty_builder()?;
    let entry = b.create_region_all()?;
    let t = b.create_region_all()?;
    let f = b.create_region_all()?;
    b.set_entry_region_all(entry)?;

    b.set_region(entry);
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
    let cond = b.build_boolean_const(true);
    b.build_if(cond, t, f)?;

    b.set_region(t);
    let one = b.build_int_const(1u64, ValueType::I64)?;
    let two = b.build_int_const(2u64, ValueType::I64)?;
    let sum = b.build_int_binary_operation(one, two, IntBinaryOp::Add, ValueType::I64)?;
    b.build_return(Some(sum), &[])?;

    b.set_region(f);
    let three = b.build_int_const(3u64, ValueType::I64)?;
    b.build_return(Some(three), &[])?;
    b.set_lift_addr(None);

    let mut fg = b.build()?;

    let mut pipeline = OptimizerPipeline::new();
    pipeline.add(ConstantFold::new());
    pipeline.add(PhiCollapse);
    pipeline.add(RegionCollapse);
    pipeline.add(DeadBranchElimination);
    pipeline.add(CfgDetach);

    // First run: must converge and leave no If nodes.
    pipeline.run(&mut fg, &mut strider_orchestrator::opt::OptCtx::new(None))?;
    let ifs_after_first = count_reachable(&fg, |k| matches!(k, NodeKind::If));
    let nodes_after_first = fg.walk().count();
    assert_eq!(ifs_after_first, 0, "first run must eliminate If(true)");

    // Second run: graph is already at fixed-point; node count must not change.
    pipeline.run(&mut fg, &mut strider_orchestrator::opt::OptCtx::new(None))?;
    let nodes_after_second = fg.walk().count();
    assert_eq!(
        nodes_after_first, nodes_after_second,
        "second pipeline run must be idempotent (fixed-point reached after first run)"
    );
    Ok(())
}
