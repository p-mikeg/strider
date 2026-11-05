//! Property-based invariants over value-only DAGs, generated as a sequence of
//! `FunctionBuilder` actions against type-tagged operand buckets.
//!
//! Value-only on purpose: control-flow invariants stay in hand-authored
//! fixtures.

#![allow(clippy::todo, clippy::enum_variant_names)]

use proptest::prelude::*;

use strider_ir::IRWalker;
use strider_ir_test_utils::proptest_gen::{replay, step_seq};

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 1000,
        .. ProptestConfig::default()
    })]

    #[test]
    fn prop_validate_always_passes(steps in step_seq()) {
        let Some(fg) = replay(&steps) else {
            return Ok(());
        };
        let res = strider_ir::validate::validate(&fg);
        prop_assert!(
            res.is_ok(),
            "validate failed on a strategy-generated graph: {:?}",
            res.err()
        );
    }

    #[test]
    fn prop_preorder_visits_each_node_at_most_once(steps in step_seq()) {
        let Some(fg) = replay(&steps) else {
            return Ok(());
        };
        use std::collections::HashSet;
        let visited: Vec<_> = fg.walk().collect();
        let unique: HashSet<_> = visited.iter().copied().collect();
        prop_assert_eq!(
            visited.len(),
            unique.len(),
            "preorder produced a duplicate visit: {:?}",
            visited
        );
    }

    /// Re-creating any cached node of a generated graph returns that node:
    /// dedup depends on the key, not on construction history.
    #[test]
    fn prop_dedup_determinism(steps in step_seq()) {
        let Some(mut fg) = replay(&steps) else {
            return Ok(());
        };
        let nodes: Vec<_> = fg.graph().all_node_ids().collect();
        let mut checked = 0;
        for n in nodes {
            if !fg.graph().is_cached(n) {
                continue;
            }
            let g = fg.graph();
            let kind = *g.node_kind(n);
            let inputs: Vec<_> = g.node_inputs(n).iter().collect();
            let outputs: Vec<_> = g.node_outputs(n).iter().map(|&v| g.value_kind(v)).collect();
            let again = fg.graph_mut().create_node(kind, inputs, outputs);
            prop_assert_eq!(again, n, "re-creating a cached node minted a new one");
            checked += 1;
        }
        prop_assert!(checked > 0, "no cached node: the generator emits none");
    }
}
