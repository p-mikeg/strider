use super::*;
use strider_ir::node::{NodeKind, NodeOutputKind, NodeOutputType};
use strider_ir::{FunctionBuilder, IntBinaryOp};
use strider_ir_test_utils::SENTINEL_LIFT_ADDR;

use crate::opt::pipeline::Optimizer;

/// `DetachUnreachable` detaches the inputs of an orphan node grafted onto
/// the arena, and reports `NoChange` (bookkeeping never escalates).
#[test]
fn detaches_orphan_inputs_reports_nochange() -> crate::opt::Result<()> {
    let mut b = FunctionBuilder::empty()?;
    let entry = b.create_region()?;
    b.set_entry_region(entry)?;
    b.set_region(entry);
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
    let c = b.build_int_const(0u64, NodeOutputType::I64)?;
    b.build_return(Some(c), &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    // Graft an unreachable Add (not consumed by anything reachable).
    let a = fg.make_int_const(1u64, NodeOutputType::I64)?;
    let bb = fg.make_int_const(2u64, NodeOutputType::I64)?;
    let orphan = fg.create_node(
        NodeKind::IntBinaryOp(IntBinaryOp::Add),
        [a, bb],
        [NodeOutputKind::OutputType(NodeOutputType::I64)],
    );
    fg.set_asm_fingerprint(orphan, vec![SENTINEL_LIFT_ADDR]);
    assert_eq!(fg.node_inputs(orphan).len(), 2, "orphan has inputs pre-sweep");

    let res = DetachUnreachable.optimize(&mut fg, &crate::opt::OptCtx::empty())?;
    assert_eq!(
        res,
        OptimizationResult::NoChange,
        "orphan detachment is bookkeeping — must report NoChange"
    );
    assert_eq!(
        fg.node_inputs(orphan).len(),
        0,
        "orphan's inputs must have been detached"
    );
    Ok(())
}

/// On an all-reachable graph the sweep is a no-op and reports `NoChange`.
#[test]
fn all_reachable_is_noop() -> crate::opt::Result<()> {
    let mut fg = strider_ir_test_utils::make_empty_fn(|b| {
        b.build_int_const(7u64, NodeOutputType::I64)
    })?;
    let res = DetachUnreachable.optimize(&mut fg, &crate::opt::OptCtx::empty())?;
    assert_eq!(res, OptimizationResult::NoChange);
    Ok(())
}
