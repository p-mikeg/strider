//! Property-based invariants for the optimizer.
//!
//! Companion to `strider-ir/tests/proptest_invariants.rs`, which verifies
//! `validate()` over strategy-generated graphs; this file verifies asm-
//! fingerprint monotonicity under the default pipeline: every node's
//! fingerprint after `opt::default_pipeline().run()` is a superset of its
//! pre-pipeline value (the contract is superset-only, passes may grow
//! fingerprints but never shrink them).
//!
//! Scope: value-only DAGs via a sequence of [`FunctionBuilder`] actions,
//! mirroring `cranelift-fuzzgen`. Control-flow properties stay in
//! hand-authored fixtures.

#![allow(clippy::todo, clippy::enum_variant_names)]

use std::collections::{BTreeSet, HashMap};

use proptest::prelude::*;

use strider_ir::node::NodeId;
use strider_ir_test_utils::proptest_gen::{replay, step_seq};
use strider_orchestrator::opt::{OptimizerPipeline, default_pipeline};

fn collect_fingerprints(function: &strider_ir::Function) -> HashMap<NodeId, Vec<u64>> {
    function
        .graph()
        .all_node_ids()
        .map(|id| {
            (
                id,
                function
                    .side_tables()
                    .asm_fingerprint(id)
                    .into_iter()
                    .collect(),
            )
        })
        .collect()
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 1000,
        .. ProptestConfig::default()
    })]

    /// Every node's asm-fingerprint after the default optimizer pipeline
    /// is a superset of its pre-pipeline fingerprint (passes may grow
    /// fingerprints, never shrink them).
    ///
    /// A node detached / unreachable after the pipeline is exempt: some
    /// passes leave zombie nodes in the arena with their inputs detached,
    /// so only nodes that survive `all_node_ids()` with a non-empty
    /// pre-fingerprint are checked.
    #[test]
    fn prop_fingerprint_monotonic_under_default_pipeline(steps in step_seq()) {
        let Some(mut fg) = replay(&steps) else {
            return Ok(());
        };

        let pre: HashMap<NodeId, Vec<u64>> = collect_fingerprints(&fg);

        let pipeline: OptimizerPipeline = default_pipeline();
        let run_res = pipeline.run(&mut fg, &mut strider_orchestrator::opt::OptCtx::new(None));
        prop_assert!(
            run_res.is_ok(),
            "default_pipeline should not error on strategy-generated graph: {:?}",
            run_res.err()
        );

        let post: HashMap<NodeId, Vec<u64>> = collect_fingerprints(&fg);

        for (id, pre_fp) in &pre {
            if pre_fp.is_empty() {
                // Exempt structural kinds have no fingerprint to grow.
                continue;
            }
            let Some(post_fp) = post.get(id) else {
                // Node removed entirely (e.g. PhiCollapse); the fingerprint
                // contract only applies to surviving nodes.
                continue;
            };
            let pre_set: BTreeSet<u64> = pre_fp.iter().copied().collect();
            let post_set: BTreeSet<u64> = post_fp.iter().copied().collect();
            prop_assert!(
                pre_set.is_subset(&post_set),
                "fingerprint shrunk at node {:?}:\n  pre  = {:?}\n  post = {:?}",
                id, pre_fp, post_fp,
            );
        }
    }

}
