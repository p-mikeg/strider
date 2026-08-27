//! Property-based invariants over value-only DAGs, generated as a sequence of
//! `FunctionBuilder` actions against type-tagged operand buckets.
//!
//! Value-only on purpose: control-flow invariants stay in hand-authored
//! fixtures.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable,
    clippy::todo,
    clippy::enum_variant_names
)]

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

    /// Dedup must be independent of construction history.
    #[test]
    fn prop_dedup_determinism(steps in step_seq()) {
        let Some(mut fg) = replay(&steps) else {
            return Ok(());
        };
        use strider_ir::node::{NodeKind, ValueKind, ValueType};
        let id = fg.intern_int_const(42, ValueType::I32);
        let a = fg.graph_mut().create_node(
            NodeKind::IntConst(id),
            [],
            [ValueKind::Typed(ValueType::I32)],
        );
        let b = fg.graph_mut().create_node(
            NodeKind::IntConst(id),
            [],
            [ValueKind::Typed(ValueType::I32)],
        );
        prop_assert_eq!(
            a, b,
            "dedup cache returned distinct NodeIds for identical IntConst(42:I32)"
        );
    }
}
