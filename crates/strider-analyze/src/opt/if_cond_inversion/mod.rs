//! `IfCondInversion` — canonicalises `If(BitNot(C)) {A} {B}` into
//! `If(C) {B} {A}` so every `If` node in the optimised IR has a non-`BitNot`
//! condition.  (`BitNot` here is the 1-bit `IntUnaryOp::BitNot` at `I1` —
//! logical not of a boolean.)
//!
//! Source-level `if (c) A else B` and `if (!c) B else A` are logically
//! equivalent, but lifters can produce either shape depending on which
//! branch direction the architecture's flag-test instruction prefers.
//! Two shapes for one semantic forces every pattern-matcher caller to
//! handle both.  This pass eagerly rewrites the `BitNot`-cond shape into
//! the canonical direct shape so [`crate::pattern::IfPat`] only needs to match
//! one layout.
//!
//! The rewrite is sound because:
//!   1. `If(BitNot(C))` takes the true branch iff `BitNot(C)` is true,
//!      iff `C` is false.
//!   2. `If(C){B}{A}` (after the rewrite) takes the true branch iff `C`
//!      is true (going to `B`), and the false branch iff `C` is false
//!      (going to `A`).  Identical control-flow semantics.
//!
//! Convergence: each application strictly removes one `BitNot` from the
//! cond input, and the inner `BitNot(BitNot(x))` shape collapses via
//! the existing `!!x → x` rule in `ConstantFold` (which we expect to run
//! first in the pipeline).  No circular rewriting.
//!
//! ## Pipeline placement
//!
//! Add to `stable_default_pipeline` after `ConstantFold` so any
//! `BitNot(BitNot(x)) → x` simplification has already collapsed
//! before we look for the canonical shape.  Without that ordering,
//! `If(BitNot(BitNot(C)))` would land in canonical form via two
//! applications instead of one — still correct, just one extra
//! fixed-point iteration.
//!
//! ## Why this is a dedicated pass and not a `crate::pattern::rewrite_rule`
//!
//! The `crate::pattern::rewrite_rule` engine doesn't currently support rewrites
//! that swap consumers across two of a node's outputs — its model is
//! "find a matching subtree, replace its single output's consumers with
//! a fresh node's output."  The cond-inversion rewrite needs:
//!   - input redirection (cond slot 1 → inner of BitNot);
//!   - bidirectional consumer swap on the two `Control` outputs.
//!
//! Both are use-list mutations the pattern-rewrite engine doesn't do, so
//! we hand-write the surgery.

use strider_ir::node::{NodeId, NodeKind};

use crate::opt::error::Result;
use crate::opt::peephole::impl_optimizer_from_peephole;
use crate::opt::pipeline::OptimizationResult;

/// Pass that rewrites `If(BitNot(C))` into `If(C)` with branches swapped.
///
/// Add to `stable_default_pipeline` after `ConstantFold` so the
/// `BitNot(BitNot) → x` rule (at `I1`) simplifies double-negations first.
#[derive(Clone)]
pub struct IfCondInversion;

impl crate::opt::peephole::PeepholePass for IfCondInversion {
    fn matches_kind(&self, kind: &NodeKind) -> bool {
        matches!(kind, NodeKind::If)
    }

    fn try_rewrite(
        &self,
        ctx: &mut crate::pattern::RewriteCtx<'_>,
        root: NodeId,
    ) -> Result<OptimizationResult> {
        if !is_inverted_cond(ctx.graph_ref(), root) {
            return Ok(OptimizationResult::NoChange);
        }
        invert(ctx.function_mut(), root)?;
        Ok(OptimizationResult::Changed)
    }

    /// Inverting an `If` swaps its control consumers but doesn't fold
    /// into a constant — re-enqueueing consumers would only re-walk
    /// joins that haven't changed shape.
    fn propagate_to_consumers(&self) -> bool {
        false
    }
}

impl_optimizer_from_peephole!(IfCondInversion);

/// Returns `true` when the `If` node's cond input (slot 1) consumes the
/// output of a logical-NOT node — an `IntUnaryOp::BitNot` whose output is
/// `I1` (a 1-bit complement, i.e. `~0 & 1 == 1` / `~1 & 1 == 0`).
fn is_inverted_cond(graph: &strider_ir::Graph, if_node: NodeId) -> bool {
    let Ok([_ctrl, cond_out]) = graph.node_inputs_exact::<2>(if_node) else {
        return false;
    };
    let cond_node = graph.node_for_output(cond_out);
    if !matches!(
        graph.node_kind(cond_node),
        NodeKind::IntUnaryOp(strider_ir::IntUnaryOp::BitNot)
    ) {
        return false;
    }
    // Only a 1-bit BitNot is a logical NOT; a wider BitNot is a bitwise
    // complement and inverting the `If` around it would change semantics.
    graph
        .output_kind(cond_out)
        .as_value()
        .is_some_and(|ty| ty.is_bool())
}

/// Performs the inversion in place:
///   1. Re-points the `If`'s cond input from `BitNot(X)` to `X`.
///   2. Swaps the consumers of the two control outputs.
fn invert(function: &mut strider_ir::Function, if_node: NodeId) -> Result<()> {
    // Redirect cond input.
    //
    // Read the BitNot node's input first, then call `update_input` on the
    // If's cond slot to consume it directly.  After this step the BitNot
    // is unreferenced from the If; its other consumers (if any) keep using
    // it, which is fine.
    let cond_input_id = function.node_input_id_at(if_node, 1)?;
    let cond_out = function.input_output_id(cond_input_id);
    let bit_not_node = function.node_for_output(cond_out);
    let [inner] = function.node_inputs_exact::<1>(bit_not_node)?;
    // Count BitNot's consumers BEFORE redirecting: if we are the only
    // user, BitNot becomes dead after the redirect and its
    // contributing-asm history needs to be absorbed by the inner-cond
    // node (the new If consumer).  When BitNot has other live uses,
    // those uses still produce the value via BitNot's own
    // fingerprint, so transferring would CONTAMINATE inner_node's
    // fingerprint with addresses that don't contribute to its value
    // (false positives violate the contract that a fingerprint names
    // the asm insns whose lifting or rewrite contributed to that
    // node's value).
    let bit_not_uses_before = function.output_uses(cond_out).count();
    function.update_input(cond_input_id, inner);
    if bit_not_uses_before == 1 {
        let inner_node = function.node_for_output(inner);
        function.extend_asm_fingerprint_from(inner_node, bit_not_node);
    }

    // Swap consumers between output[0] (true) and output[1] (false).
    //
    // Both outputs share the same producer node (`if_node`), and each output
    // has its own use-list.  `output_uses` yields `(consumer_node, input_idx)`
    // pairs; resolve each to a stable `NodeInputId` before mutating, since
    // `update_input` rewrites the use-list and would invalidate any
    // half-consumed iterator.  Collect both lists before any redirect.
    let [true_out, false_out] = function.node_outputs_exact::<2>(if_node)?;
    let true_use_ids: smallvec::SmallVec<[strider_ir::node::NodeInputId; 4]> = function
        .output_uses(true_out)
        .map(|(consumer, idx)| function.node_input_id_at(consumer, idx as usize))
        .collect::<Result<_>>()?;
    let false_use_ids: smallvec::SmallVec<[strider_ir::node::NodeInputId; 4]> = function
        .output_uses(false_out)
        .map(|(consumer, idx)| function.node_input_id_at(consumer, idx as usize))
        .collect::<Result<_>>()?;
    for use_id in true_use_ids {
        function.update_input(use_id, false_out);
    }
    for use_id in false_use_ids {
        function.update_input(use_id, true_out);
    }
    Ok(())
}

#[cfg(test)]
mod tests;
