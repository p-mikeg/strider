//! Integration tests for [`opt::stable_default_pipeline`] and
//! [`opt::destructive_default_pipeline`].
//!
//! These tests exercise the pipeline-shape contracts the indirect-branch
//! fixed-point loop depends on:
//!
//!   * the **stable** subset must NOT include passes that detach phi /
//!     ControlState / If nodes (those are unsafe to run mid-iteration —
//!     they invalidate the strider `RegionIrCache`),
//!   * the **destructive** subset must include exactly those passes,
//!   * `default_pipeline()` is shape-equivalent to "stable then destructive".
//!
//! The tests don't run the pipelines on real graphs (that's covered by
//! the per-pass test suites and the strider-side optimizer_pipeline_subsets
//! tests).  Here we pin the *registration* contract — that the right
//! passes land in the right buckets, by name.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use opt::{
    default_pipeline, destructive_default_pipeline, stable_default_pipeline, OptimizerPipeline,
};

fn pass_names(p: &OptimizerPipeline) -> Vec<String> {
    p.optimizer_names().iter().map(|s| (*s).to_string()).collect()
}

#[test]
fn stable_subset_does_not_include_redundant_phis() {
    // RedundantPhis is destructive: it removes phi nodes by detaching
    // their inputs and rewiring consumers past them.  The fixed-point
    // loop's `RegionIrCache` pins phi `NodeId`s at lift time and adds
    // new predecessors as inputs to those existing nodes.  If
    // RedundantPhis ran mid-iteration, the next iteration's
    // predecessor-extension would dangle (or worse, attach to a
    // detached node).  So the stable subset must NOT contain it.
    let names = pass_names(&stable_default_pipeline());
    assert!(
        !names.iter().any(|n| n.contains("RedundantPhis")),
        "stable_default_pipeline contains RedundantPhis: {names:?}"
    );
}

#[test]
fn stable_subset_does_not_include_dead_branch_elimination() {
    // DeadBranchElimination removes If nodes whose condition is a
    // BoolConst.  A later iteration may make the condition phi-
    // dependent again (when a new predecessor brings a different
    // constant), but the branch is already gone.  Same cache-
    // invalidation story as RedundantPhis.
    let names = pass_names(&stable_default_pipeline());
    assert!(
        !names.iter().any(|n| n.contains("DeadBranchElimination")),
        "stable_default_pipeline contains DeadBranchElimination: {names:?}"
    );
}

#[test]
fn stable_subset_includes_constant_fold_and_known_bits() {
    // The two rewrite-only passes that the spec marks ✓ for
    // mid-iteration use AND that take no caller-supplied state
    // (so they can be added with no arguments here).  Pinning their
    // presence prevents a future refactor from accidentally moving
    // them to the destructive bucket.
    //
    // `LoadReadOnly` is also categorised as stable in the spec but
    // requires a caller-supplied ROM image — strider's
    // `build_optimizer_pipeline` (the real callsite) layers it on
    // top of this subset.
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
    // The two passes the spec explicitly bans from intermediate
    // iterations.  The destructive subset must include both — it's
    // what the orchestrator runs once the fixed point is reached.
    let names = pass_names(&destructive_default_pipeline());
    for required in ["RedundantPhis", "DeadBranchElimination"] {
        assert!(
            names.iter().any(|n| n.contains(required)),
            "destructive subset missing {required}: {names:?}"
        );
    }
}

#[test]
fn full_pipeline_pass_count_equals_stable_plus_destructive() {
    // Pinning the equivalence "stable + destructive == full" keeps
    // existing default_pipeline() callers behaviourally unchanged
    // while the orchestrator splits the run.
    let stable_count = stable_default_pipeline().optimizer_count();
    let destructive_count = destructive_default_pipeline().optimizer_count();
    let full_count = default_pipeline().optimizer_count();
    assert_eq!(stable_count + destructive_count, full_count,);
}

#[test]
fn full_pipeline_passes_are_stable_then_destructive_in_order() {
    // The combined pipeline must list the stable passes first
    // (so a single fixed-point loop run is equivalent to the previous
    // single-pipeline behaviour).
    let stable_names = pass_names(&stable_default_pipeline());
    let destructive_names = pass_names(&destructive_default_pipeline());
    let full_names = pass_names(&default_pipeline());
    let mut expected = stable_names.clone();
    expected.extend_from_slice(&destructive_names);
    assert_eq!(full_names, expected);
}

// ── Python ↔ Rust pipeline-shape sync ──
//
// The Python wrapper `strider_py::opt::PipelineState` rebuilds the three
// named pipelines manually (it can't directly clone a Rust pipeline because
// `Box<dyn Optimizer>` isn't re-extractable through PyO3).  Drift between
// the manual Python list and the Rust factory functions silently produces
// graphs that look different on the two paths.
//
// The two assertions below pin the invariant: any Rust-side change that
// adds/drops a pass must also update the Python side, or this test fails.
// The Python side has corresponding `pass_count()` accessors that a Python
// test can call and compare against these expected numbers.

#[test]
fn default_pipeline_pass_count_pinned() {
    // Bumping this number requires updating `PipelineState::from_default`
    // in `crates/strider-py/src/opt.rs` to mirror the new pass list.
    assert_eq!(default_pipeline().optimizer_count(), 6);
}

#[test]
fn stable_default_pipeline_pass_count_pinned() {
    assert_eq!(stable_default_pipeline().optimizer_count(), 4);
}

#[test]
fn destructive_default_pipeline_pass_count_pinned() {
    assert_eq!(destructive_default_pipeline().optimizer_count(), 2);
}
