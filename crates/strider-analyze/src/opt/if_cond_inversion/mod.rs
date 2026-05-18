//! `IfCondInversion` — canonicalises `If(BoolUnaryOp::Neg(C)) {A} {B}` into
//! `If(C) {B} {A}` so every `If` node in the optimised IR has a non-`BoolNeg`
//! condition.
//!
//! Source-level `if (c) A else B` and `if (!c) B else A` are logically
//! equivalent, but lifters can produce either shape depending on which
//! branch direction the architecture's flag-test instruction prefers.
//! Two shapes for one semantic forces every pattern-matcher caller to
//! handle both.  This pass eagerly rewrites the `BoolNeg`-cond shape into
//! the canonical direct shape so [`pattern::IfPat`] only needs to match
//! one layout.
//!
//! The rewrite is sound because:
//!   1. `If(BoolNeg(C))` takes the true branch iff `BoolNeg(C)` is true,
//!      iff `C` is false.
//!   2. `If(C){B}{A}` (after the rewrite) takes the true branch iff `C`
//!      is true (going to `B`), and the false branch iff `C` is false
//!      (going to `A`).  Identical control-flow semantics.
//!
//! Convergence: each application strictly removes one `BoolNeg` from the
//! cond input, and the inner `BoolNeg(BoolNeg(x))` shape collapses via
//! the existing `!!x → x` rule in `ConstantFold` (which we expect to run
//! first in the pipeline).  No circular rewriting.
//!
//! ## Pipeline placement
//!
//! Add to `stable_default_pipeline` after `ConstantFold` so any
//! `BoolNeg(BoolNeg(x)) → x` simplification has already collapsed
//! before we look for the canonical shape.  Without that ordering,
//! `If(BoolNeg(BoolNeg(C)))` would land in canonical form via two
//! applications instead of one — still correct, just one extra
//! fixed-point iteration.
//!
//! ## Why this is a dedicated pass and not a `pattern::rewrite_rule`
//!
//! The `pattern::rewrite_rule` engine doesn't currently support rewrites
//! that swap consumers across two of a node's outputs — its model is
//! "find a matching subtree, replace its single output's consumers with
//! a fresh node's output."  The cond-inversion rewrite needs:
//!   - input redirection (cond slot 1 → inner of BoolNeg);
//!   - bidirectional consumer swap on the two `Control` outputs.
//!
//! Both are use-list mutations the pattern-rewrite engine doesn't do, so
//! we hand-write the surgery.

use strider_ir::node::{NodeId, NodeKind};

use crate::opt::error::Result;
use crate::opt::pipeline::{OptimizationResult, Optimizer};

/// Pass that rewrites `If(BoolNeg(C))` into `If(C)` with branches swapped.
///
/// Add to `stable_default_pipeline` after `ConstantFold` so the
/// `BoolNeg(BoolNeg) → x` rule simplifies double-negations first.
pub struct IfCondInversion;

impl Optimizer for IfCondInversion {
    fn optimize(&self, ctx: &mut pattern::RewriteCtx<'_>) -> Result<OptimizationResult> {
        // Collect candidate `If` nodes whose cond input is BoolUnaryOp::Neg.
        // We filter here (not in `preorder_kind`) because we need to read
        // the input chain too.
        let graph = ctx.graph_ref();
        let candidates: Vec<NodeId> = ctx
            .preorder_kind(|k| matches!(k, NodeKind::If))
            .filter(|&node| is_inverted_cond(graph, node))
            .collect();

        let mut result = OptimizationResult::NoChange;
        for if_node in candidates {
            invert(ctx.graph_mut(), if_node)?;
            result = OptimizationResult::Changed;
        }
        Ok(result)
    }
}

/// Returns `true` when the `If` node's cond input (slot 1) consumes the
/// output of a `BoolUnaryOp::Neg` node.
fn is_inverted_cond(graph: &strider_ir::Graph, if_node: NodeId) -> bool {
    let Ok([_ctrl, cond_out]) = graph.node_inputs_exact::<2>(if_node) else {
        return false;
    };
    let cond_node = graph.get_node_from_output(cond_out);
    matches!(
        graph.node_kind(cond_node),
        NodeKind::BoolUnaryOp(strider_ir::BoolUnaryOp::Neg)
    )
}

/// Performs the inversion in place:
///   1. Re-points the `If`'s cond input from `BoolNeg(X)` to `X`.
///   2. Swaps the consumers of the two control outputs.
fn invert(graph: &mut strider_ir::Graph, if_node: NodeId) -> Result<()> {
    // Step 1: redirect cond input.
    //
    // Read the BoolNeg node's input first, then call `update_input` on the
    // If's cond slot to consume it directly.  After this step the BoolNeg
    // is unreferenced from the If; its other consumers (if any) keep using
    // it, which is fine.
    let cond_input_id = graph.node_input_id_at(if_node, 1)?;
    let cond_out = graph.input_output_id(cond_input_id);
    let bool_neg_node = graph.get_node_from_output(cond_out);
    let [inner] = graph.node_inputs_exact::<1>(bool_neg_node)?;
    // Absorb the BoolNeg's asm-fingerprint into the surviving inner-cond
    // node BEFORE redirecting the input, so the contributing-asm history
    // survives even when the BoolNeg becomes dead (no other consumers).
    // This upholds the asm-fingerprint superset contract: a rewrite that
    // makes a node dead must transfer its fingerprint to whatever node
    // takes over its semantic role.
    let inner_node = graph.get_node_from_output(inner);
    graph.extend_asm_fingerprint_from(inner_node, bool_neg_node);
    graph.update_input(cond_input_id, inner);

    // Step 2: swap consumers between output[0] (true) and output[1] (false).
    //
    // Both outputs share the same producer node (`if_node`), and each output
    // has its own use-list.  `output_uses` yields `(consumer_node, input_idx)`
    // pairs; resolve each to a stable `NodeInputId` before mutating, since
    // `update_input` rewrites the use-list and would invalidate any
    // half-consumed iterator.  Collect both lists before any redirect.
    let [true_out, false_out] = graph.node_outputs_exact::<2>(if_node)?;
    let true_use_ids: smallvec::SmallVec<[strider_ir::node::NodeInputId; 4]> = graph
        .output_uses(true_out)
        .map(|(consumer, idx)| graph.node_input_id_at(consumer, idx as usize))
        .collect::<Result<_>>()?;
    let false_use_ids: smallvec::SmallVec<[strider_ir::node::NodeInputId; 4]> = graph
        .output_uses(false_out)
        .map(|(consumer, idx)| graph.node_input_id_at(consumer, idx as usize))
        .collect::<Result<_>>()?;
    for use_id in true_use_ids {
        graph.update_input(use_id, false_out);
    }
    for use_id in false_use_ids {
        graph.update_input(use_id, true_out);
    }
    Ok(())
}

#[cfg(test)]
mod tests;
