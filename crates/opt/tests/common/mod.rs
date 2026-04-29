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
use ir::{BuiltFunctionGraph, FunctionBuilder, Value};
use opt::{Error, Result};

/// Builds a single-region function whose return value is what `f` produces.
pub fn make_fn<F>(f: F) -> Result<BuiltFunctionGraph>
where
    F: FnOnce(&mut FunctionBuilder) -> Result<Value>,
{
    let mut b = FunctionBuilder::new_raw(vec![], &[], &[], &[], None, 0)?;
    let region = b.create_region()?;
    b.set_entry_region(region)?;
    b.set_region(region);
    let val = f(&mut b)?;
    b.build_return(Some(val), &[])?;
    Ok(b.build()?)
}

/// Builds a single-region function with a tracked variable `vn`. The closure
/// receives the read-back value (a `ControlPhi` over `InitialVar(vn)`).
pub fn make_fn_with_var<F>(
    vn: rsleigh::Vn,
    f: F,
) -> Result<(BuiltFunctionGraph, Value)>
where
    F: FnOnce(&mut FunctionBuilder, Value) -> Result<Value>,
{
    let mut b = FunctionBuilder::new_raw(vec![vn], &[vn], &[], &[], None, 0)?;
    let region = b.create_region()?;
    b.set_entry_region(region)?;
    b.set_region(region);
    let x = b.read_variable(&vn)?;
    let val = f(&mut b, x)?;
    b.build_return(Some(val), &[])?;
    Ok((b.build()?, x))
}

/// The output id that the (unique) Return node receives as its value
/// argument (input[2]: input[0]=ctrl, input[1]=mem).
pub fn return_value(fg: &BuiltFunctionGraph) -> Result<Value> {
    let ret = fg
        .all_node_ids()
        .find(|&n| matches!(fg.graph.node_kind(n), NodeKind::Return))
        .ok_or(opt::ErrorKind::NoReturnNode)?;
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

/// Fabricates a register varnode of the given size at offset `off`.
pub fn reg_vn(off: u64, size: u32) -> rsleigh::Vn {
    rsleigh::Vn {
        size,
        addr: rsleigh::VnAddr {
            off,
            space: rsleigh::VnSpace::REGISTER,
        },
    }
}

/// Stack-pointer varnode at REGISTER:0x20, size 4 (matches x86 ESP).
pub fn sp_vn() -> rsleigh::Vn {
    reg_vn(0x20, 4)
}

/// Runs `pass.optimize` until it reports `NoChange` or `MAX_ITERS` is hit.
/// Returns `AssertionFailed` if `MAX_ITERS` is exceeded (a non-converging
/// pass).
pub fn run_to_fixed_point<P: opt::Optimizer>(
    pass: &P,
    fg: &mut BuiltFunctionGraph,
) -> Result<()> {
    const MAX_ITERS: usize = 100;
    for _ in 0..MAX_ITERS {
        if !pass.optimize(&mut fg.graph, fg.entry)?.changed() {
            return Ok(());
        }
    }
    Err(Error::from(opt::ErrorKind::AssertionFailed(
        format!("pass did not converge in {MAX_ITERS} iterations"),
    )))
}

// Re-export commonly used IR types so test files don't need long use-paths.
pub use ir::node::NodeOutputType as Type;
