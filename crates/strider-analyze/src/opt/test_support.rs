//! Re-exports of shared mock-IR helpers for white-box tests inside `opt`.
//!
//! The mock-IR builders themselves live in the dedicated
//! `strider-ir-test-utils` crate so every consumer (this crate's
//! `#[cfg(test)] mod tests` blocks, strider-ir's own builder tests,
//! strider's integration tests, strider-analyze's proptest) shares one
//! canonical implementation.
//!
//! Also hosts `count` / `return_value` / `return_kind` /
//! `find_unique_if` — bookkeeping helpers that white-box
//! (`src/<pass>/tests.rs`) and black-box (`tests/<file>.rs`) suites
//! both use.  Each helper takes `&Graph` directly rather than a
//! `RewriteCtxView` — the helpers never mutated and only ever read
//! kind / inputs / outputs, all of which live on `Graph`.  The
//! formerly-needed `count_reachable` helper was observably equivalent
//! to `Graph::count_kind` and has been retired in favour of the
//! canonical accessor.

#![allow(dead_code)] // Helpers reused across files; not every caller uses every one.

use anyhow::anyhow;

use strider_ir::node::{NodeId, NodeKind};
use strider_ir::{Graph, Value};
use strider_target::Endianness;

pub(crate) use strider_ir_test_utils::{make_empty_fn as make_fn, make_fn_with_var};

use crate::opt::{ConstantFold, OptimizerPipeline, RedundantPhis, StackLoadForward};

/// Builds the canonical 3-pass optimizer pipeline used across the
/// opt white-box tests: `ConstantFold` → `RedundantPhis` →
/// `StackLoadForward(sp, endianness)`.
///
/// `sp` is the stack-pointer varnode for the fixture's target;
/// `endianness` matches the fixture's IR.  Tests that need a
/// different subset of passes still build their own pipeline directly
/// — this helper exists to retire the 14× verbatim copy of the
/// sequence.
pub(crate) fn standard_test(sp: rsleigh::Vn, endianness: Endianness) -> OptimizerPipeline {
    let mut pipeline = OptimizerPipeline::new();
    pipeline.add(ConstantFold);
    pipeline.add(RedundantPhis);
    pipeline.add(StackLoadForward::new(sp, endianness));
    pipeline
}

/// The output id that the (unique) Return node receives as its value
/// argument (input[2]: input[0]=ctrl, input[1]=mem).
pub(crate) fn return_value(graph: &Graph) -> crate::opt::Result<Value> {
    let ret = graph
        .all_node_ids()
        .find(|&n| matches!(graph.node_kind(n), NodeKind::Return))
        .ok_or_else(|| anyhow!("no return node found in function"))?;
    Ok(graph.node_inputs(ret)[2])
}

/// `NodeKind` of the return-value producer.
pub(crate) fn return_kind(graph: &Graph) -> crate::opt::Result<NodeKind> {
    let val = return_value(graph)?;
    Ok(*graph.kind_of_output(val))
}

/// Counts nodes matching `pred` (full arena, including detached zombies).
///
/// For the reachable-only count use [`Graph::count_kind`] directly —
/// it walks via `preorder` and filters by kind in one O(n) sweep.
pub(crate) fn count<F: Fn(&NodeKind) -> bool>(graph: &Graph, pred: F) -> usize {
    graph
        .all_node_ids()
        .filter(|&n| pred(graph.node_kind(n)))
        .count()
}

/// Locates the unique `If` node in `graph`.  Panics if zero or more than
/// one is present — both indicate a fixture-construction bug.
pub(crate) fn find_unique_if(graph: &Graph) -> NodeId {
    let mut iter = graph
        .all_node_ids()
        .filter(|&n| matches!(graph.node_kind(n), NodeKind::If));
    let first = iter.next().expect("test fixture must contain an If node");
    assert!(
        iter.next().is_none(),
        "test fixture has more than one If node",
    );
    first
}
