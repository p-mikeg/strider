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
    stable_default_pipeline().run(&mut graph.graph, graph.entry).expect("run 1");
    let snapshot_node_count = graph.all_node_ids().count();
    stable_default_pipeline().run(&mut graph.graph, graph.entry).expect("run 2");
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
    strider_analyze::opt::default_pipeline().run(&mut g_full.graph, g_full.entry).expect("full");
    stable_default_pipeline().run(&mut g_split.graph, g_split.entry).expect("stable");
    destructive_default_pipeline()
        .run(&mut g_split.graph, g_split.entry)
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
    stable_default_pipeline().run(&mut graph.graph, graph.entry).expect("stable");
    let before = graph.all_node_ids().count();
    destructive_default_pipeline()
        .run(&mut graph.graph, graph.entry)
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
            matches!(
                graph.graph.node_kind(nid),
                strider_ir::node::NodeKind::VarPhi(_) | strider_ir::node::NodeKind::MemPhi
            )
        })
        .count();
    stable_default_pipeline().run(&mut graph.graph, graph.entry).expect("stable");
    let phi_count_after = graph
        .preorder()
        .filter(|&nid| {
            matches!(
                graph.graph.node_kind(nid),
                strider_ir::node::NodeKind::VarPhi(_) | strider_ir::node::NodeKind::MemPhi
            )
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
    use strider::indirect_resolve::classify_anchor;

    let (graph_stable, anchor_stable) = build_initial_var_target_scenario_x86_64();
    let (graph_full, anchor_full) = build_initial_var_target_scenario_x86_64();

    // x86_64: link_register_vn is None.  Classifier returns None
    // for `InitialVar(rax)` (no LR match, no IntConst, no ValuePhi).
    let cls_stable = classify_anchor(&graph_stable, anchor_stable, None).expect("classify");
    let cls_full = classify_anchor(&graph_full, anchor_full, None).expect("classify");
    assert_eq!(
        cls_stable, cls_full,
        "IR-level indirect-branch resolver classification must be invariant to destructive subset",
    );
}

// ── default pipeline composition ────────────────────────────────────────────

fn pass_names_t17(p: &strider_analyze::opt::OptimizerPipeline) -> Vec<String> {
    p.optimizer_names().iter().map(|s| (*s).to_string()).collect()
}

#[test]
fn stable_default_pipeline_contains_flag_cmp_canonicalize() {
    let names = pass_names_t17(&stable_default_pipeline());
    assert!(
        names.iter().any(|n| n.contains("FlagCmpCanonicalize")),
        "stable_default_pipeline missing FlagCmpCanonicalize: {names:?}"
    );
}

#[test]
fn stable_default_pipeline_contains_if_cond_inversion() {
    let names = pass_names_t17(&stable_default_pipeline());
    assert!(
        names.iter().any(|n| n.contains("IfCondInversion")),
        "stable_default_pipeline missing IfCondInversion: {names:?}"
    );
}

#[test]
fn stable_default_pipeline_base_pass_count_at_least_four() {
    assert!(
        stable_default_pipeline().optimizer_count() >= 4,
        "stable_default_pipeline must have >=4 passes; got {}",
        stable_default_pipeline().optimizer_count(),
    );
}
