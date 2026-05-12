//! Shared helpers for `opt` integration tests.
//!
//! Currently re-implements the patterns spread across the per-pass white-box
//! test modules so black-box `tests/*.rs` files can write concise scenarios.

#![allow(dead_code)] // Helpers are reused across files; rustc can't see all uses.
#![allow(unused_imports)] // Re-exports and helpers may not all be used in every test file.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable
)]

use ir::node::{NodeKind, NodeOutputType};
use ir::{BuiltFunctionGraph, Value};
use anyhow::anyhow;
use opt::Result;

pub use ir::test_utils::{make_empty_fn as make_fn, make_fn_with_var, reg_vn, sp_vn_x86 as sp_vn};

/// The output id that the (unique) Return node receives as its value
/// argument (input[2]: input[0]=ctrl, input[1]=mem).
pub fn return_value(fg: &BuiltFunctionGraph) -> Result<Value> {
    let ret = fg
        .all_node_ids()
        .find(|&n| matches!(fg.graph.node_kind(n), NodeKind::Return))
        .ok_or_else(|| anyhow!("no return node found in function"))?;
    Ok(fg.graph.node_inputs(ret)[2])
}

/// `NodeKind` of the return-value producer.
pub fn return_kind(fg: &BuiltFunctionGraph) -> Result<NodeKind> {
    let val = return_value(fg)?;
    Ok(*fg.graph.kind_of_output(val))
}

/// Counts nodes matching `pred`.
pub fn count<F: Fn(&NodeKind) -> bool>(fg: &BuiltFunctionGraph, pred: F) -> usize {
    fg.all_node_ids()
        .filter(|&n| pred(fg.graph.node_kind(n)))
        .count()
}

/// Counts CFG-reachable nodes matching `pred`.
pub fn count_reachable<F: Fn(&NodeKind) -> bool>(
    fg: &BuiltFunctionGraph,
    pred: F,
) -> usize {
    let reachable: std::collections::HashSet<_> = fg.preorder().collect();
    fg.all_node_ids()
        .filter(|n| reachable.contains(n))
        .filter(|&n| pred(fg.graph.node_kind(n)))
        .count()
}

/// Runs `pass.optimize` until it reports `NoChange` or `MAX_ITERS` is hit.
/// Returns `AssertionFailed` if `MAX_ITERS` is exceeded (a non-converging
/// pass).
pub fn run_to_fixed_point<P: opt::OptimizerRaw>(
    pass: &P,
    fg: &mut BuiltFunctionGraph,
) -> Result<()> {
    const MAX_ITERS: usize = 100;
    for _ in 0..MAX_ITERS {
        if !pass.optimize_raw(&mut fg.graph, fg.entry)?.changed() {
            return Ok(());
        }
    }
    Err(anyhow!("assertion failed: pass did not converge in {MAX_ITERS} iterations"))
}

// Re-export commonly used IR types so test files don't need long use-paths.
pub use ir::node::NodeOutputType as Type;
