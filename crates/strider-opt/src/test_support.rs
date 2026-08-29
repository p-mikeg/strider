use anyhow::anyhow;

use strider_ir::node::{NodeId, NodeKind};
use strider_ir::{Graph, IRViewer, Value};

pub(crate) use strider_ir_test_utils::{make_empty_fn as make_fn, make_fn_with_var};

use crate::{ConstantFold, LoadForward, Optimizer, OptimizerPipeline, PhiCollapse, RegionCollapse};

/// Runs `pass` until a run reports no change, each iteration with a fresh
/// default `OptCtx`. Returns the number of iterations that DID report a
/// change, so `0` means the first run was already a no-op.
pub(crate) fn run_to_fixed_point(
    pass: &dyn Optimizer,
    fg: &mut strider_ir::Function,
) -> crate::Result<usize> {
    let mut iterations = 0;
    while crate::run_one(pass, fg, &mut crate::OptCtx::new(None))?.changed() {
        iterations += 1;
    }
    Ok(iterations)
}

#[track_caller]
pub(crate) fn assert_return_kind(graph: &Graph, expected: NodeKind) {
    let got = return_kind(graph).expect("function must return a value");
    assert_eq!(got, expected, "return-value producer kind mismatch");
}

#[track_caller]
pub(crate) fn assert_returns_const(f: &strider_ir::Function, expected: u64) {
    let val = return_value(f.graph()).expect("function must return a value");
    let got = f.int_const_u128(val);
    assert_eq!(
        got,
        Some(u128::from(expected)),
        "return-value must be IntConst({expected:#x})"
    );
}

/// `ConstantFold` + `PhiCollapse` + `RegionCollapse`.
pub(crate) fn cf_rp_pipeline() -> OptimizerPipeline {
    let mut p = OptimizerPipeline::new();
    p.add(ConstantFold::new());
    p.add(PhiCollapse);
    p.add(RegionCollapse);
    p
}

/// `cf_rp_pipeline` plus `LoadForward`.
pub(crate) fn standard_test() -> OptimizerPipeline {
    let mut pipeline = OptimizerPipeline::new();
    pipeline.add(ConstantFold::new());
    pipeline.add(PhiCollapse);
    pipeline.add(RegionCollapse);
    pipeline.add(LoadForward::default());
    pipeline
}

/// An [`crate::OptCtx`] with `stack_global_disjoint` cleared. The other
/// default-on claim, `assume_incoming_args_survive_calls`, stays set.
pub(crate) fn octx_structural_only() -> crate::OptCtx<'static> {
    let mut ctx = crate::OptCtx::new(None);
    ctx.options.assumptions.stack_global_disjoint = false;
    ctx
}

/// An [`crate::OptCtx`] at the pipeline default, `stack_global_disjoint` set.
pub(crate) fn octx_stack_global_disjoint() -> crate::OptCtx<'static> {
    let mut ctx = crate::OptCtx::new(None);
    ctx.options.assumptions.stack_global_disjoint = true;
    ctx
}

/// Input[2] of the first `Return`, after ctrl and mem.
pub(crate) fn return_value(graph: &Graph) -> crate::Result<Value> {
    let ret = graph
        .all_node_ids()
        .find(|&n| matches!(graph.node_kind(n), NodeKind::Return))
        .ok_or_else(|| anyhow!("no return node found in function"))?;
    Ok(graph.node_inputs(ret)[2])
}

pub(crate) fn return_kind(graph: &Graph) -> crate::Result<NodeKind> {
    let val = return_value(graph)?;
    Ok(*graph.kind_of_value(val))
}

/// Panics on zero or multiple `If` nodes; both mean the fixture is wrong.
pub(crate) fn find_unique_if(graph: &Graph) -> NodeId {
    let mut iter = graph
        .all_node_ids()
        .filter(|&n| matches!(graph.node_kind(n), NodeKind::If));
    let first = iter.next().expect("test fixture must contain an If node");
    assert!(
        iter.next().is_none(),
        "test fixture has more than one If node",
    );
    first
}
