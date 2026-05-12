//! Re-exports of shared mock-IR helpers for white-box tests inside `opt`.
//!
//! These live in `ir::test_utils` (feature-gated) so all crates that build
//! mock IR for testing share one canonical implementation.
//!
//! Also hosts `count` / `count_reachable` / `return_value` / `return_kind`
//! — the bookkeeping helpers that white-box (`src/<pass>/tests.rs`) and
//! black-box (`tests/<file>.rs`) suites both use.  Promoted from a
//! per-test-file inline implementation — the same logic was
//! duplicated 14× across opt's white-box test modules.

#![allow(dead_code)] // Helpers reused across files; not every caller uses every one.

use anyhow::anyhow;

use ir::node::{NodeId, NodeKind};
use ir::Value;

pub(crate) use ir::test_utils::{make_empty_fn as make_fn, make_fn_with_var};

/// The output id that the (unique) Return node receives as its value
/// argument (input[2]: input[0]=ctrl, input[1]=mem).
pub(crate) fn return_value(ctx: pattern::RewriteCtxView<'_>) -> crate::Result<Value> {
    let ret = ctx
        .all_node_ids()
        .find(|&n| matches!(ctx.node_kind(n), NodeKind::Return))
        .ok_or_else(|| anyhow!("no return node found in function"))?;
    Ok(ctx.node_inputs(ret)[2])
}

/// `NodeKind` of the return-value producer.
pub(crate) fn return_kind(ctx: pattern::RewriteCtxView<'_>) -> crate::Result<NodeKind> {
    let val = return_value(ctx)?;
    Ok(*ctx.kind_of_output(val))
}

/// Counts nodes matching `pred` (full arena, including detached zombies).
pub(crate) fn count<F: Fn(&NodeKind) -> bool>(ctx: pattern::RewriteCtxView<'_>, pred: F) -> usize {
    ctx.all_node_ids()
        .filter(|&n| pred(ctx.node_kind(n)))
        .count()
}

/// Counts CFG-reachable nodes matching `pred` — the form most tests
/// actually want (zombies left by `RedundantPhis` etc. don't count).
pub(crate) fn count_reachable<F: Fn(&NodeKind) -> bool>(
    ctx: pattern::RewriteCtxView<'_>,
    pred: F,
) -> usize {
    let reachable: entity_utils::DenseEntitySet<NodeId> = ctx.preorder().collect();
    ctx.all_node_ids()
        .filter(|n| reachable.contains(*n))
        .filter(|&n| pred(ctx.node_kind(n)))
        .count()
}

/// Locates the unique `If` node in `ctx`.  Panics if zero or more than
/// one is present — both indicate a fixture-construction bug.
pub(crate) fn find_unique_if(ctx: pattern::RewriteCtxView<'_>) -> NodeId {
    let mut iter = ctx
        .all_node_ids()
        .filter(|&n| matches!(ctx.node_kind(n), NodeKind::If));
    let first = iter.next().expect("test fixture must contain an If node");
    assert!(
        iter.next().is_none(),
        "test fixture has more than one If node",
    );
    first
}
