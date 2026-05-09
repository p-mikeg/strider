//! Re-exports of shared mock-IR helpers for white-box tests inside `opt`.
//!
//! These live in `ir::test_utils` (feature-gated) so all crates that build
//! mock IR for testing share one canonical implementation.
//!
//! Also hosts `count` / `count_reachable` / `return_value` / `return_kind`
//! — the bookkeeping helpers that white-box (`src/<pass>/tests.rs`) and
//! black-box (`tests/<file>.rs`) suites both use.  Promoted from the
//! per-test-file inline implementation flagged by
//! `reviews/round8-repetition-sweep.md` (#1) — the same logic was
//! duplicated 14× across opt's white-box test modules.

#![allow(dead_code)] // Helpers reused across files; not every caller uses every one.

use anyhow::anyhow;

use ir::node::{NodeId, NodeKind};
use ir::{BuiltFunctionGraph, Value};

pub(crate) use ir::test_utils::{make_empty_fn as make_fn, make_fn_with_var};

/// The output id that the (unique) Return node receives as its value
/// argument (input[2]: input[0]=ctrl, input[1]=mem).
pub(crate) fn return_value(fg: &BuiltFunctionGraph) -> crate::Result<Value> {
    let ret = fg
        .all_node_ids()
        .find(|&n| matches!(fg.graph.node_kind(n), NodeKind::Return))
        .ok_or_else(|| anyhow!("no return node found in function"))?;
    Ok(fg.graph.node_inputs(ret)[2])
}

/// `NodeKind` of the return-value producer.
pub(crate) fn return_kind(fg: &BuiltFunctionGraph) -> crate::Result<NodeKind> {
    let val = return_value(fg)?;
    Ok(*fg.graph.kind_of_output(val))
}

/// Counts nodes matching `pred` (full arena, including detached zombies).
pub(crate) fn count<F: Fn(&NodeKind) -> bool>(fg: &BuiltFunctionGraph, pred: F) -> usize {
    fg.all_node_ids()
        .filter(|&n| pred(fg.graph.node_kind(n)))
        .count()
}

/// Counts CFG-reachable nodes matching `pred` — the form most tests
/// actually want (zombies left by `RedundantPhis` etc. don't count).
pub(crate) fn count_reachable<F: Fn(&NodeKind) -> bool>(
    fg: &BuiltFunctionGraph,
    pred: F,
) -> usize {
    let reachable: entity_utils::DenseEntitySet<NodeId> = fg.preorder().collect();
    fg.all_node_ids()
        .filter(|n| reachable.contains(*n))
        .filter(|&n| pred(fg.graph.node_kind(n)))
        .count()
}
