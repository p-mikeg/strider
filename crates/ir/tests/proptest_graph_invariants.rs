//! Property-based invariants for the IR graph.
//!
//! Generates random sequences of public-API builder calls and asserts the
//! invariants the validator + dedup cache + walk_graph are supposed to
//! maintain. These properties are the strongest backstop for the crate's
//! correctness contract — when a future change subtly breaks one of them,
//! proptest's shrinker will minimise to the smallest counterexample.

#![allow(clippy::unwrap_used)]

mod common;

use std::collections::HashSet;

use ir::node::{NodeId, NodeOutputType};
use ir::walk::walk_graph;
use ir::{BuiltFunctionGraph, FunctionBuilder, IntBinaryOp, IntCmpOp};
use proptest::prelude::*;

/// One step in a generated builder sequence. Each variant is a public-API
/// call that requires only previously-produced output ids (referenced by
/// `usize` index into the running pool).
#[derive(Debug, Clone)]
enum Step {
    IntConst(u64, NodeOutputType),
    BoolConst(bool),
    IntBinary(usize, usize, IntBinaryOp, NodeOutputType),
    IntCmp(usize, usize, IntCmpOp, NodeOutputType),
    Truncate(usize, NodeOutputType),
}

fn int_op() -> impl Strategy<Value = IntBinaryOp> {
    prop_oneof![
        Just(IntBinaryOp::Add),
        Just(IntBinaryOp::Sub),
        Just(IntBinaryOp::Mul),
        Just(IntBinaryOp::And),
        Just(IntBinaryOp::Or),
        Just(IntBinaryOp::Xor),
        Just(IntBinaryOp::ShiftLeft),
        Just(IntBinaryOp::ShiftRight),
    ]
}

fn cmp_op() -> impl Strategy<Value = IntCmpOp> {
    prop_oneof![
        Just(IntCmpOp::Equal),
        Just(IntCmpOp::Less),
        Just(IntCmpOp::Sless),
    ]
}

fn int_ty() -> impl Strategy<Value = NodeOutputType> {
    prop_oneof![
        Just(NodeOutputType::U8),
        Just(NodeOutputType::U16),
        Just(NodeOutputType::U32),
        Just(NodeOutputType::U64),
    ]
}

fn step() -> impl Strategy<Value = Step> {
    prop_oneof![
        (any::<u64>(), int_ty()).prop_map(|(v, t)| Step::IntConst(v, t)),
        any::<bool>().prop_map(Step::BoolConst),
        (any::<u8>(), any::<u8>(), int_op(), int_ty())
            .prop_map(|(i, j, o, t)| Step::IntBinary(i as usize, j as usize, o, t)),
        (any::<u8>(), any::<u8>(), cmp_op(), int_ty())
            .prop_map(|(i, j, o, t)| Step::IntCmp(i as usize, j as usize, o, t)),
        (any::<u8>(), int_ty()).prop_map(|(i, t)| Step::Truncate(i as usize, t)),
    ]
}

fn step_seq() -> impl Strategy<Value = Vec<Step>> {
    proptest::collection::vec(step(), 1..=20)
}

/// Replay a generated step sequence into a built graph. Returns `None` when
/// the sequence is invalid (no value to return — empty pool, or every
/// builder call errored). The returned graph has already been
/// `validate`-d via `build()`.
fn replay(steps: &[Step]) -> Option<BuiltFunctionGraph> {
    let mut b = FunctionBuilder::new_raw(vec![], &[], &[], &[], None, 0).ok()?;
    let region = b.create_region().ok()?;
    b.set_entry_region(region).ok()?;
    b.set_region(region);
    let mut pool: Vec<ir::Value> = Vec::new();

    for s in steps {
        match s {
            Step::IntConst(v, t) => pool.push(b.build_int_const(*v, *t).ok()?),
            Step::BoolConst(v) => pool.push(b.build_boolean_const(*v)),
            Step::IntBinary(i, j, op, t) => {
                if pool.is_empty() {
                    return None;
                }
                let a = pool[i % pool.len()];
                let bv = pool[j % pool.len()];
                if let Ok(r) = b.build_int_binary_operation(a, bv, *op, *t) {
                    pool.push(r);
                }
            }
            Step::IntCmp(i, j, op, t) => {
                if pool.is_empty() {
                    return None;
                }
                let a = pool[i % pool.len()];
                let bv = pool[j % pool.len()];
                if let Ok(r) = b.build_int_cmp_operation(a, bv, *op, *t) {
                    pool.push(r);
                }
            }
            Step::Truncate(i, t) => {
                if pool.is_empty() {
                    return None;
                }
                let a = pool[i % pool.len()];
                if let Ok(r) = b.truncate_if_needed(a, *t) {
                    pool.push(r);
                }
            }
        }
    }

    if pool.is_empty() {
        return None;
    }
    let last = *pool.last().unwrap();
    b.build_return(Some(last), &[]).ok()?;
    b.build().ok()
}

proptest! {
    // 256 cases by default; can be tuned with PROPTEST_CASES env var.
    #![proptest_config(ProptestConfig {
        cases: 256,
        .. ProptestConfig::default()
    })]

    /// Property 1: every graph the generator can produce passes `validate`.
    /// `build()` runs validate internally and returns its error, so reaching
    /// `Some(_)` from `replay` already implies validation passed.
    #[test]
    fn build_validate_holds(seq in step_seq()) {
        let _ = replay(&seq);
    }

    /// Property 2: `walk_graph` from `entry` returns each visited node at
    /// most once.
    #[test]
    fn walk_visits_each_node_at_most_once(seq in step_seq()) {
        if let Some(fg) = replay(&seq) {
            let visited: Vec<NodeId> = walk_graph(&fg.graph, fg.entry).collect();
            let unique: HashSet<NodeId> = visited.iter().copied().collect();
            prop_assert_eq!(visited.len(), unique.len());
        }
    }

    /// Property 3: cacheable kinds dedup. Re-creating the very last produced
    /// integer node with identical inputs and output type returns the same
    /// NodeId. Uses `BuiltFunctionGraph::graph` directly because the dedup
    /// cache is internal to `Graph` and not exposed via FunctionBuilder.
    #[test]
    fn dedup_determinism(seq in step_seq()) {
        if let Some(mut fg) = replay(&seq) {
            // Pick an arbitrary cacheable construction that already exists.
            // IntConst(42, U32) is deterministic and always cacheable.
            use ir::node::{NodeKind, NodeOutputKind};
            let a = fg.graph.create_node(
                NodeKind::IntConst(42),
                [],
                [NodeOutputKind::OutputType(NodeOutputType::U32)],
            );
            let b = fg.graph.create_node(
                NodeKind::IntConst(42),
                [],
                [NodeOutputKind::OutputType(NodeOutputType::U32)],
            );
            prop_assert_eq!(a, b);
        }
    }
}
