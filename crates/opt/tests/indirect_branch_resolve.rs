//! Integration tests for the [`opt::IndirectBranchResolve`] pass.
//!
//! These tests live in `crates/opt/tests/` (rather than alongside the
//! pass in `src/indirect_branch_resolve/`) so they exercise the pass
//! through the public `opt` crate's API exactly like a downstream
//! consumer would: build a graph, register the pass in a pipeline,
//! run, observe.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use ir::FunctionBuilder;
use ir::node::{NodeKind, NodeOutputType};
use opt::{IndirectBranchResolve, OptimizerPipeline};

/// Build a minimal placeholder graph whose `IndirectBranch`'s
/// value-input is `IntConst(target)` — the same shape strider lifts
/// for an `UnresolvedIndirectBranch` placeholder after `mov rax, K;
/// jmp *rax` folds.
fn placeholder_graph_with_int_const(
    target: u64,
) -> (ir::Graph, ir::node::NodeId, ir::node::NodeOutputId) {
    let mut b = FunctionBuilder::empty().unwrap();
    let region = b.create_region().unwrap();
    b.set_entry_region(region).unwrap();
    b.set_region(region);
    let anchor = b.build_int_const(target, NodeOutputType::U64).unwrap();
    b.build_indirect_branch(anchor).unwrap();
    let built = b.build().unwrap();
    let entry = built.entry;
    (built.graph, entry, anchor)
}

/// The pass runs inside an [`OptimizerPipeline`] (the same harness
/// every other opt pass uses) and applies the in-place edit.  Pins
/// the integration contract: an `IndirectBranchResolve` instance is
/// a drop-in `Optimizer` and the pipeline runs it without
/// special-casing.
#[test]
fn pass_runs_inside_optimizer_pipeline() {
    let (mut graph, entry, anchor) = placeholder_graph_with_int_const(0xc0de);

    let mut pass = IndirectBranchResolve::new();
    pass.unresolved_anchors.push((
        opt::AnchorAddr {
            machine_addr: 0x1000,
            insn_index: 0,
        },
        anchor,
    ));
    // `0xc0de` is "out of function range" — apply tail-call in-place.
    pass.is_tail_call = Box::new(|target| target == 0xc0de);

    let mut pipeline = OptimizerPipeline::new();
    pipeline.add(pass);
    pipeline
        .run(&mut graph, entry)
        .expect("pipeline run must succeed");

    // The pipeline materialised a Call node — the in-place tail-call
    // edit fired.
    let mut had_call = false;
    for nid in graph.all_node_ids() {
        if matches!(graph.node_kind(nid), NodeKind::Call) {
            had_call = true;
            break;
        }
    }
    assert!(
        had_call,
        "pipeline-driven IndirectBranchResolve must materialise a Call node",
    );
}

/// Running the pass twice (a no-op call after the first edit) does
/// not corrupt the graph.  Pins the soundness contract: once an
/// anchor's placeholder is replaced, the pass's re-classification
/// yields no live anchor and the second run is a no-op.  Round-
/// trips the orchestrator's "apply edits + re-run stable subset"
/// iteration shape — the same anchor list cannot fire the same edit
/// twice.
#[test]
fn pass_round_trips_through_existing_orchestrator() {
    let (mut graph, entry, anchor) = placeholder_graph_with_int_const(0xc0de);

    let mut pass = IndirectBranchResolve::new();
    pass.unresolved_anchors.push((
        opt::AnchorAddr {
            machine_addr: 0x2000,
            insn_index: 0,
        },
        anchor,
    ));
    pass.is_tail_call = Box::new(|target| target == 0xc0de);

    // First run: applies the edit.
    use opt::Optimizer;
    let r1 = pass
        .optimize(&mut graph, entry)
        .expect("first run must succeed");
    assert!(matches!(r1, opt::OptimizationResult::Changed));

    // Second run: anchor's `IndirectBranch` placeholder is gone
    // (replaced by a Call → fresh Return), so the classifier has
    // nothing to reclassify and the pass returns NoChange.  Confirms
    // a re-run on the same anchor list is idempotent (the
    // orchestrator's stable subset re-runs the pass after each
    // iteration).
    let r2 = pass
        .optimize(&mut graph, entry)
        .expect("second run must succeed");
    assert!(matches!(r2, opt::OptimizationResult::NoChange));

    // ir::validate must succeed on the post-edit graph — round-trip
    // soundness.
    ir::validate::validate(&graph, entry).expect("validate must pass after edit");
}
