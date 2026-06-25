//! Re-exports of shared mock-IR helpers for white-box tests inside `opt`.
//!
//! The mock-IR builders themselves live in the dedicated
//! `strider-ir-test-utils` crate so every consumer (this crate's
//! `#[cfg(test)] mod tests` blocks, strider-ir's own builder tests,
//! strider's integration tests, strider-orchestrator's proptest) shares one
//! canonical implementation.
//!
//! Also hosts `count` / `return_value` / `return_kind` /
//! `find_unique_if` — bookkeeping helpers that white-box
//! (`src/<pass>/tests.rs`) and black-box (`tests/<file>.rs`) suites
//! both use.  Each helper takes `&Graph` directly rather than a
//! rewrite context — the helpers never mutated and only ever read
//! kind / inputs / outputs, all of which live on `Graph`.  The
//! formerly-needed `count_reachable` helper was observably equivalent
//! to `Graph::count_kind` and has been retired in favour of the
//! canonical accessor.

#![allow(dead_code)] // Helpers reused across files; not every caller uses every one.

use anyhow::anyhow;

use strider_ir::node::{NodeId, NodeKind};
use strider_ir::{Graph, IRViewer, Value};

pub(crate) use strider_ir_test_utils::{make_empty_fn as make_fn, make_fn_with_var};

use crate::{ConstantFold, LoadForward, Optimizer, OptimizerPipeline, PhiCollapse, RegionCollapse};

/// Runs `pass` repeatedly (each iteration through the free
/// [`crate::run_one`] with a fresh default `OptCtx::new(None)`, matching
/// the verbatim `let mut changed = true; while changed { … }` loops this
/// helper replaces) until a run reports no change.  Returns the number of
/// iterations that DID report a change — `0` means the first run was
/// already a no-op.
pub(crate) fn run_to_fixed_point(
    pass: &dyn Optimizer,
    fg: &mut strider_ir::Function,
) -> crate::Result<usize> {
    let mut iterations = 0;
    while crate::run_one(pass, fg, &mut crate::OptCtx::new(None))?.changed() {
        iterations += 1;
    }
    Ok(iterations)
}

/// Asserts the return-value producer's [`NodeKind`] is exactly `expected`
/// (the `assert_eq!(return_kind(g)?, …)` shape, minus the boilerplate).
#[track_caller]
pub(crate) fn assert_return_kind(graph: &Graph, expected: NodeKind) {
    let got = return_kind(graph).expect("function must return a value");
    assert_eq!(got, expected, "return-value producer kind mismatch");
}

/// Asserts the function returns an `IntConst` with the given value —
/// the most common post-fold assertion across the pass test suites.
#[track_caller]
pub(crate) fn assert_returns_const(f: &strider_ir::Function, expected: u64) {
    let val = return_value(f.graph()).expect("function must return a value");
    let got = f.int_const_val(val);
    assert_eq!(
        got,
        Some(expected),
        "return-value must be IntConst({expected:#x})"
    );
}

/// Returns a fresh pipeline containing exactly `ConstantFold` +
/// `PhiCollapse` + `RegionCollapse` — the most common phi-collapsing
/// pairing used across the stack / memory / arg white-box test suites
/// (replacing the former `ConstantFold` + `RedundantPhis`).  Callers
/// that need additional passes (e.g. `add_post_pass(FunctionArgDetect)`)
/// chain those calls on the returned pipeline.
///
/// Only replace the verbatim `new() → add(CF) → add(phi-collapse)`
/// sequences with this helper — pipelines that include
/// `StackOffsetDetect`, `DeadBranchElimination`, or other passes must
/// still build their own pipeline explicitly.
pub(crate) fn cf_rp_pipeline() -> OptimizerPipeline {
    let mut p = OptimizerPipeline::new();
    p.add(ConstantFold::new());
    p.add(PhiCollapse);
    p.add(RegionCollapse);
    p
}

/// Builds the canonical optimizer pipeline used across the opt white-box
/// tests: `ConstantFold` → `PhiCollapse` → `RegionCollapse` →
/// `LoadForward`.
///
/// `LoadForward` reads the stack pointer and endianness from the function
/// under analysis, so the fixture must bake those in.  Tests that need a
/// different subset of passes still build their own pipeline directly
/// — this helper exists to retire the 14× verbatim copy of the
/// sequence.
pub(crate) fn standard_test() -> OptimizerPipeline {
    let mut pipeline = OptimizerPipeline::new();
    pipeline.add(ConstantFold::new());
    pipeline.add(PhiCollapse);
    pipeline.add(RegionCollapse);
    pipeline.add(LoadForward);
    pipeline
}

/// Shared [`crate::OptCtx`] carrying the conservative
/// [`crate::AliasMode::Strict`] floor.  The alias precision now lives on
/// the per-run `OptCtx` (global), not on the pass, so a strict-mode test
/// runs [`standard_test`] with this ctx instead of a strict-pass variant.
pub(crate) fn octx_strict() -> crate::OptCtx<'static> {
    let mut ctx = crate::OptCtx::new(None);
    ctx.options.alias_mode = crate::AliasMode::Strict;
    ctx
}

/// Shared [`crate::OptCtx`] carrying the permissive
/// [`crate::AliasMode::StackGlobalDisjoint`] mode (the default).  Kept
/// explicit for tests that assert the aggressive cross-class
/// step-through behaviour.
pub(crate) fn octx_permissive() -> crate::OptCtx<'static> {
    let mut ctx = crate::OptCtx::new(None);
    ctx.options.alias_mode = crate::AliasMode::StackGlobalDisjoint;
    ctx
}

/// The output id that the (unique) Return node receives as its value
/// argument (input[2]: input[0]=ctrl, input[1]=mem).
pub(crate) fn return_value(graph: &Graph) -> crate::Result<Value> {
    let ret = graph
        .all_node_ids()
        .find(|&n| matches!(graph.node_kind(n), NodeKind::Return))
        .ok_or_else(|| anyhow!("no return node found in function"))?;
    Ok(graph.node_inputs(ret)[2])
}

/// `NodeKind` of the return-value producer.
pub(crate) fn return_kind(graph: &Graph) -> crate::Result<NodeKind> {
    let val = return_value(graph)?;
    Ok(*graph.kind_of_value(val))
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
