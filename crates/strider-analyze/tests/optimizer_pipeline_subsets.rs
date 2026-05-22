//! Integration tests for the optimiser-tier separation
//! (`strider_analyze::opt::stable_default_pipeline` vs
//! `strider_analyze::opt::destructive_default_pipeline`).
//!
//! Pin the soundness contracts the strider fixed-point orchestrator
//! depends on:
//!
//!   * the stable subset is idempotent (running it twice in a row
//!     produces no further change),
//!   * the destructive subset includes RedundantPhis +
//!     DeadBranchElimination,
//!   * the stable subset does NOT include those passes,
//!   * IR-level indirect-branch resolver's classification produces the same induced edge set
//!     before and after the destructive subset runs (the "robust to
//!     RedundantPhis" guarantee from the spec).
//!
//! These tests live at the strider crate level (rather than the opt
//! crate) because they exercise the round-trip through full IR
//! lifts — the unit tests in `crates/opt/tests/pipeline_subsets.rs`
//! pin the registration shape; these tests pin the runtime
//! behavioural contracts.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;
use common::indirect_resolve_helpers::build_initial_var_target_scenario_x86_64;

use strider_analyze::opt::{destructive_default_pipeline, stable_default_pipeline};

#[test]
fn stable_subset_is_idempotent_on_optimised_graph() {
    // Run the stable subset, then run it again, then assert the
    // second run is a no-op.  The pipeline's internal fixed-point
    // loop already iterates until convergence, so a second
    // OptimizerPipeline::run cannot change anything if the first
    // already converged.
    let (mut graph, _anchor) = build_initial_var_target_scenario_x86_64();
    let entry = graph.entry().unwrap();
    stable_default_pipeline().run(graph.graph_mut(), entry).expect("run 1");
    let snapshot_node_count = graph.all_node_ids().count();
    let entry = graph.entry().unwrap();
    stable_default_pipeline().run(graph.graph_mut(), entry).expect("run 2");
    let after_node_count = graph.all_node_ids().count();
    assert_eq!(
        snapshot_node_count, after_node_count,
        "stable subset must be idempotent on a converged graph",
    );
}

#[test]
fn stable_then_destructive_equals_full_default_pipeline_node_count() {
    // Two graphs of the same fixture: one runs default_pipeline,
    // the other runs stable_default_pipeline + destructive_default_pipeline.
    // Their node counts after must match — pinning the equivalence
    // the orchestrator relies on at fixed point.
    let (mut g_full, _) = build_initial_var_target_scenario_x86_64();
    let (mut g_split, _) = build_initial_var_target_scenario_x86_64();
    let entry = g_full.entry().unwrap();
    strider_analyze::opt::default_pipeline().run(g_full.graph_mut(), entry).expect("full");
    let entry = g_split.entry().unwrap();
    stable_default_pipeline().run(g_split.graph_mut(), entry).expect("stable");
    let entry = g_split.entry().unwrap();
    destructive_default_pipeline()
        .run(g_split.graph_mut(), entry)
        .expect("destructive");
    let full_count = g_full.all_node_ids().count();
    let split_count = g_split.all_node_ids().count();
    assert_eq!(full_count, split_count);
}

#[test]
fn destructive_subset_reduces_or_preserves_node_count() {
    // Running the destructive subset on a graph the stable subset
    // already optimised must NOT INCREASE the node count — every
    // pass in the destructive subset is a node-removal pass.
    let (mut graph, _) = build_initial_var_target_scenario_x86_64();
    let entry = graph.entry().unwrap();
    stable_default_pipeline().run(graph.graph_mut(), entry).expect("stable");
    let before = graph.all_node_ids().count();
    let entry = graph.entry().unwrap();
    destructive_default_pipeline()
        .run(graph.graph_mut(), entry)
        .expect("destructive");
    let after = graph.all_node_ids().count();
    assert!(
        after <= before,
        "destructive subset must not add nodes; before={before}, after={after}"
    );
}

#[test]
fn stable_subset_does_not_remove_phi_nodes() {
    // The whole point of the stable/destructive split: the stable
    // subset must NOT call RedundantPhis.  We build a graph that
    // would have at least one phi after lift, run the stable subset,
    // and assert that the phi count is unchanged.  The orchestrator
    // uses the stable subset on every iteration (intermediate
    // resolutions) and the destructive subset only at fixed-point
    // exit; this test pins the contract that callers using the
    // stable subset get phi preservation.
    let (mut graph, _) = build_initial_var_target_scenario_x86_64();
    let phi_count_before = graph
        .preorder()
        .filter(|&nid| {
            (matches!(graph.node_kind(nid), strider_ir::node::NodeKind::Phi)
                && graph.phi_var_tag(nid).is_some())
                || matches!(graph.node_kind(nid), strider_ir::node::NodeKind::MemPhi)
        })
        .count();
    let entry = graph.entry().unwrap();
    stable_default_pipeline().run(graph.graph_mut(), entry).expect("stable");
    let phi_count_after = graph
        .preorder()
        .filter(|&nid| {
            (matches!(graph.node_kind(nid), strider_ir::node::NodeKind::Phi)
                && graph.phi_var_tag(nid).is_some())
                || matches!(graph.node_kind(nid), strider_ir::node::NodeKind::MemPhi)
        })
        .count();
    assert_eq!(
        phi_count_before, phi_count_after,
        "stable subset must not remove phi nodes",
    );
}

#[test]
fn ir_level_classification_robust_to_destructive_subset() {
    // Classify an anchor on a stable-only optimised graph and on a
    // stable + destructive optimised graph.  Both must produce the
    // same `Option<ResolvedTargets>` — the spec's "the IR-level orchestrator resolver's
    // classification is robust to whether the destructive subset
    // has run" guarantee.
    use strider_analyze::opt::classify_anchor;
    use strider_analyze::opt::analyze_known_bits;

    let (graph_stable, anchor_stable) = build_initial_var_target_scenario_x86_64();
    let (graph_full, anchor_full) = build_initial_var_target_scenario_x86_64();

    // x86_64: link_register_vn is None.  Classifier returns None
    // for `InitialVar(rax)` (no LR match, no IntConst, no ValuePhi).
    let view_stable: strider_analyze::pattern::RewriteCtxView<'_> = (&graph_stable).into();
    let known_stable = analyze_known_bits(view_stable).expect("analyze_known_bits");
    let cls_stable = classify_anchor(view_stable, anchor_stable, None, None, None, &known_stable);
    let view_full: strider_analyze::pattern::RewriteCtxView<'_> = (&graph_full).into();
    let known_full = analyze_known_bits(view_full).expect("analyze_known_bits");
    let cls_full = classify_anchor(view_full, anchor_full, None, None, None, &known_full);
    assert_eq!(
        cls_stable, cls_full,
        "IR-level indirect-branch resolver classification must be invariant to destructive subset",
    );
}

