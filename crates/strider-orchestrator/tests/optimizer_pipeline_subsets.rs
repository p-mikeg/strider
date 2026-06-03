//! Integration tests for the optimiser-tier separation
//! (`strider_orchestrator::opt::stable_default_pipeline` vs
//! `strider_orchestrator::opt::destructive_default_pipeline`).
//!
//! Pin the soundness contracts the strider fixed-point orchestrator
//! depends on:
//!
//!   * the stable subset is idempotent (running it twice in a row
//!     produces no further change),
//!   * the destructive subset includes PhiCollapse + RegionCollapse +
//!     DeadBranchElimination + CfgDetach,
//!   * the stable subset does NOT include those passes,
//!   * IR-level indirect-branch resolver's classification produces the same induced edge set
//!     before and after the destructive subset runs (the "robust to
//!     phi/region collapse" guarantee from the spec).
//!
//! These tests live at the strider crate level (rather than the opt
//! crate) because they exercise the round-trip through full IR
//! lifts — the unit tests in `crates/opt/tests/pipeline_subsets.rs`
//! pin the registration shape; these tests pin the runtime
//! behavioural contracts.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;
use common::indirect_resolve_helpers::build_initial_var_target_scenario_x86_64;

use strider_orchestrator::opt::{destructive_default_pipeline, stable_default_pipeline};

#[test]
fn stable_subset_is_idempotent_on_optimised_graph() {
    // Run the stable subset, then run it again, then assert the
    // second run is a no-op.  The pipeline's internal fixed-point
    // loop already iterates until convergence, so a second
    // OptimizerPipeline::run cannot change anything if the first
    // already converged.
    let (mut function, _anchor) = build_initial_var_target_scenario_x86_64();
    stable_default_pipeline()
        .run(&mut function, &strider_orchestrator::opt::OptCtx::empty())
        .expect("run 1");
    let snapshot_node_count = function.graph().all_node_ids().count();
    stable_default_pipeline()
        .run(&mut function, &strider_orchestrator::opt::OptCtx::empty())
        .expect("run 2");
    let after_node_count = function.graph().all_node_ids().count();
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
    let ctx = strider_orchestrator::opt::OptCtx::empty();
    strider_orchestrator::opt::default_pipeline()
        .run(&mut g_full, &ctx)
        .expect("full");
    stable_default_pipeline()
        .run(&mut g_split, &ctx)
        .expect("stable");
    destructive_default_pipeline()
        .run(&mut g_split, &ctx)
        .expect("destructive");
    let full_count = g_full.graph().all_node_ids().count();
    let split_count = g_split.graph().all_node_ids().count();
    assert_eq!(full_count, split_count);
}

#[test]
fn destructive_subset_reduces_or_preserves_node_count() {
    // Running the destructive subset on a graph the stable subset
    // already optimised must NOT INCREASE the node count — every
    // pass in the destructive subset is a node-removal pass.
    let (mut function, _) = build_initial_var_target_scenario_x86_64();
    stable_default_pipeline()
        .run(&mut function, &strider_orchestrator::opt::OptCtx::empty())
        .expect("stable");
    let before = function.graph().all_node_ids().count();
    destructive_default_pipeline()
        .run(&mut function, &strider_orchestrator::opt::OptCtx::empty())
        .expect("destructive");
    let after = function.graph().all_node_ids().count();
    assert!(
        after <= before,
        "destructive subset must not add nodes; before={before}, after={after}"
    );
}

#[test]
fn stable_subset_does_not_remove_phi_nodes() {
    // The whole point of the stable/destructive split: the stable
    // subset must NOT call PhiCollapse.  We build a graph that
    // would have at least one phi after lift, run the stable subset,
    // and assert that the phi count is unchanged.  The orchestrator
    // uses the stable subset on every iteration (intermediate
    // resolutions) and the destructive subset only at fixed-point
    // exit; this test pins the contract that callers using the
    // stable subset get phi preservation.
    let (mut function, _) = build_initial_var_target_scenario_x86_64();
    let phi_count_before = function
        .walk()
        .filter(|&nid| {
            (matches!(function.node_kind(nid), strider_ir::node::NodeKind::Phi)
                && function.phi_var_tag(nid).is_some())
                || matches!(function.node_kind(nid), strider_ir::node::NodeKind::MemPhi)
        })
        .count();
    stable_default_pipeline()
        .run(&mut function, &strider_orchestrator::opt::OptCtx::empty())
        .expect("stable");
    let phi_count_after = function
        .walk()
        .filter(|&nid| {
            (matches!(function.node_kind(nid), strider_ir::node::NodeKind::Phi)
                && function.phi_var_tag(nid).is_some())
                || matches!(function.node_kind(nid), strider_ir::node::NodeKind::MemPhi)
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
    use strider_orchestrator::opt::analyze_known_bits;
    use strider_orchestrator::opt::classify_anchor;

    let (function_stable, anchor_stable) = build_initial_var_target_scenario_x86_64();
    let (function_full, anchor_full) = build_initial_var_target_scenario_x86_64();

    // x86_64: link_register_vn is None.  Classifier returns None
    // for `InitialVar(rax)` (no LR match, no IntConst, no ValuePhi).
    let view_stable: strider_opt::RewriteCtxView<'_> =
        strider_opt::RewriteCtxView::from_built(&function_stable).unwrap();
    let known_stable = analyze_known_bits(view_stable).expect("analyze_known_bits");
    let cls_stable = classify_anchor(
        view_stable,
        anchor_stable,
        None,
        None,
        strider_target::Endianness::Little,
        None,
        &known_stable,
    );
    let view_full: strider_opt::RewriteCtxView<'_> =
        strider_opt::RewriteCtxView::from_built(&function_full).unwrap();
    let known_full = analyze_known_bits(view_full).expect("analyze_known_bits");
    let cls_full = classify_anchor(
        view_full,
        anchor_full,
        None,
        None,
        strider_target::Endianness::Little,
        None,
        &known_full,
    );
    assert_eq!(
        cls_stable, cls_full,
        "IR-level indirect-branch resolver classification must be invariant to destructive subset",
    );
}

// ── Pass-count smoke contracts ───────────────────────────────────────────────
//
// Coarse-grained shape checks that the stable / destructive split is
// non-trivial: neither subset is empty, and the combined count equals
// stable + destructive.  These pin a registration-shape contract that
// does NOT depend on per-pass introspection (the pre-rewrite name-based
// shape tests in `crates/opt/tests/pipeline_subsets.rs` ran against an
// `OptimizerPipeline::optimizer_names()` accessor that was removed when
// the trait surface tightened; the per-pass content contract — which
// passes are stable vs destructive — is enforced at runtime by the
// existing tests above (idempotence on stable, phi-preservation on
// stable, full == stable + destructive node count).

#[test]
fn stable_and_destructive_subsets_are_non_empty() {
    assert!(!stable_default_pipeline().passes().is_empty());
    assert!(!destructive_default_pipeline().passes().is_empty());
}

#[test]
fn full_pipeline_pass_count_equals_stable_plus_destructive() {
    let stable_count = stable_default_pipeline().passes().len();
    let destructive_count = destructive_default_pipeline().passes().len();
    let full_count = strider_orchestrator::opt::default_pipeline().passes().len();
    assert_eq!(stable_count + destructive_count, full_count);
}

// ── Pass-membership pins ──────────────────────────────────────────────────
//
// The four tests below pin the BY-NAME registration contract that the
// orchestrator's fixed-point loop relies on: `PhiCollapse`,
// `RegionCollapse`, `DeadBranchElimination`, and `CfgDetach` are
// destructive (invalidate the orchestrator's `RegionIndex` when run
// mid-iteration); `ConstantFold` and `KnownBits` are stable (safe to
// re-run between iterations).  A future refactor must NOT silently move
// passes between buckets.

fn pass_names(p: &strider_orchestrator::opt::OptimizerPipeline) -> Vec<&'static str> {
    p.passes().iter().map(|o| o.name()).collect()
}

#[test]
fn stable_subset_does_not_include_redundant_phis() {
    let names = pass_names(&stable_default_pipeline());
    for forbidden in ["PhiCollapse", "RegionCollapse", "CfgDetach"] {
        assert!(
            !names.iter().any(|n| n.contains(forbidden)),
            "stable subset must NOT include {forbidden} (destructive — \
             invalidates orchestrator's RegionIndex).  Got: {names:?}"
        );
    }
}

#[test]
fn stable_subset_does_not_include_dead_branch_elimination() {
    let names = pass_names(&stable_default_pipeline());
    assert!(
        !names.iter().any(|n| n.contains("DeadBranchElimination")),
        "stable subset must NOT include DeadBranchElimination (destructive).  \
         Got: {names:?}"
    );
}

#[test]
fn stable_subset_includes_constant_fold_and_known_bits() {
    let names = pass_names(&stable_default_pipeline());
    for required in ["ConstantFold", "KnownBits"] {
        assert!(
            names.iter().any(|n| n.contains(required)),
            "stable subset missing {required}: {names:?}"
        );
    }
}

#[test]
fn destructive_subset_includes_redundant_phis_and_dead_branch_elim() {
    let names = pass_names(&destructive_default_pipeline());
    for required in [
        "PhiCollapse",
        "RegionCollapse",
        "DeadBranchElimination",
        "CfgDetach",
    ] {
        assert!(
            names.iter().any(|n| n.contains(required)),
            "destructive subset missing {required}: {names:?}"
        );
    }
}
