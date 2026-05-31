//! `IfCondInversion` — canonicalises `If(Xor(C, IntConst(1))) {A} {B}` into
//! `If(C) {B} {A}` so every `If` node in the optimised IR has a non-inverted
//! condition.  The inverted-cond shape is the canonical lifter output for
//! 1-bit logical NOT (`Xor(_, IntConst(1)):I1`) — the former BitNot unary-op
//! was removed in favour of `Xor(_, all_ones)` everywhere.
//!
//! Source-level `if (c) A else B` and `if (!c) B else A` are logically
//! equivalent, but lifters can produce either shape depending on which
//! branch direction the architecture's flag-test instruction prefers.
//! Two shapes for one semantic forces every pattern-matcher caller to
//! handle both.  This pass eagerly rewrites the `Xor(_, 1)`-cond shape
//! into the canonical direct shape so [`crate::pattern::IfPat`] only
//! needs to match one layout.
//!
//! The rewrite is sound because:
//!   1. `If(Xor(C, 1))` takes the true branch iff `C ^ 1` is true,
//!      iff `C` is false.
//!   2. `If(C){B}{A}` (after the rewrite) takes the true branch iff `C`
//!      is true (going to `B`), and the false branch iff `C` is false
//!      (going to `A`).  Identical control-flow semantics.
//!
//! Convergence: each application strictly removes one `Xor`-with-1 from
//! the cond input, and the inner `Xor(Xor(x, 1), 1)` shape collapses via
//! the existing `x ^ K1 ^ K2 → x ^ (K1 ^ K2)` reassoc rule in
//! `ConstantFold` (yielding `Xor(x, 0)` which then folds to `x`).  No
//! circular rewriting.
//!
//! ## Pipeline placement
//!
//! Add to `stable_default_pipeline` after `ConstantFold` so any chained
//! `Xor(_, 1)` simplification has already collapsed before we look for
//! the canonical shape.  Without that ordering, the doubly-inverted form
//! would land in canonical form via two applications instead of one —
//! still correct, just one extra fixed-point iteration.
//!
//! ## Why this is a dedicated pass and not a `crate::pattern::rewrite_rule`
//!
//! The `crate::pattern::rewrite_rule` engine doesn't currently support rewrites
//! that swap consumers across two of a node's outputs — its model is
//! "find a matching subtree, replace its single output's consumers with
//! a fresh node's output."  The cond-inversion rewrite needs:
//!   - input redirection (cond slot 1 → inner of Xor);
//!   - bidirectional consumer swap on the two `Control` outputs.
//!
//! Both are use-list mutations the pattern-rewrite engine doesn't do, so
//! we hand-write the surgery.

use std::sync::LazyLock;

use strider_ir::node::{NodeId, NodeKind, NodeOutputId};

use crate::opt::error::Result;
use crate::opt::peephole::impl_optimizer_from_peephole;
use crate::opt::pipeline::OptimizationResult;
use crate::pattern::{Capture, Matcher, Pat, bool_not, var};

/// Pass that rewrites `If(Xor(C, IntConst(1)):I1)` into `If(C)` with branches
/// swapped.
///
/// Add to `stable_default_pipeline` after `ConstantFold` so chained
/// `Xor(_, 1)` reassoc simplifies double-negations first.
#[derive(Clone)]
pub struct IfCondInversion;

/// Captured `x` slot of the `bool_not(var(x))` pattern that
/// [`is_inverted_cond_match`] matches against.  Allocated once at
/// process start so every match reuses the same `Capture` slot.
static INNER_CAPTURE: LazyLock<Capture> = LazyLock::new(Capture::new);

/// `bool_not(var(x))` — the pattern matched against an `If`'s cond input
/// producer.  `bool_not` builds the canonical `Xor(_, IntConst(1)):I1`
/// shape (since the former BitNot unary-op was removed in favour of
/// `Xor(_, all_ones)`); the `var(x)` slot binds the Xor's non-constant
/// operand to [`INNER_CAPTURE`] so the caller can substitute it for the
/// `If`'s cond input.
static INNER_PAT: LazyLock<Pat> = LazyLock::new(|| bool_not(var(*INNER_CAPTURE)));

impl crate::opt::peephole::PeepholePass for IfCondInversion {
    fn matches_kind(&self, kind: &NodeKind) -> bool {
        matches!(kind, NodeKind::If)
    }

    fn try_rewrite(
        &self,
        ctx: &mut crate::pattern::RewriteCtx<'_>,
        root: NodeId,
    ) -> Result<OptimizationResult> {
        let function = ctx.function_mut();
        let Some(inner_out) = is_inverted_cond_match(function, root) else {
            return Ok(OptimizationResult::NoChange);
        };
        invert(function, root, inner_out)?;
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

/// Returns `Some(inner_out)` when the `If` node's cond input is the
/// canonical 1-bit logical NOT shape — an `Xor(x, IntConst(1)):I1` — as
/// matched by the [`bool_not(var(x))`](INNER_PAT) pattern.  The bound
/// capture is the Xor's non-constant operand `x`, which the caller
/// substitutes for the cond input.
///
/// Why a pattern matcher rather than a hand-rolled check: the
/// `bool_not` pattern builder already encapsulates the canonical
/// logical-NOT shape (commutative Xor with the I1 all-ones constant,
/// `IntConst(1):I1`) and the I1-output guard.  Routing through the
/// matcher means the LHS shape stays in sync with the rest of the
/// pattern DSL automatically — if the canonical logical-NOT shape ever
/// changes, only the `bool_not` builder needs updating.
fn is_inverted_cond_match(
    function: &strider_ir::Function,
    if_node: NodeId,
) -> Option<NodeOutputId> {
    let [_ctrl, cond_out] = function.node_inputs_exact::<2>(if_node).ok()?;
    let cond_node = function.node_for_output(cond_out);
    // `match_at` is the single-node entry point: try the pattern at
    // exactly the cond's producer node (not a full graph walk).
    let m = Matcher::try_new(function).ok()?;
    let hit = m.match_at(cond_node, &INNER_PAT)?;
    hit.output(*INNER_CAPTURE)
}

/// Performs the inversion in place:
///   1. Re-points the `If`'s cond input from the `Xor(X, 1)` output to `X`.
///   2. Swaps the consumers of the two control outputs.
fn invert(
    function: &mut strider_ir::Function,
    if_node: NodeId,
    inner: strider_ir::node::NodeOutputId,
) -> Result<()> {
    // Redirect cond input.
    //
    // After this step the Xor is unreferenced from the If; its other
    // consumers (if any) keep using it, which is fine.
    let cond_input_id = function.node_input_id_at(if_node, 1)?;
    let cond_out = function.input_output_id(cond_input_id);
    let xor_node = function.node_for_output(cond_out);
    // Count Xor's consumers BEFORE redirecting: if we are the only
    // user, the Xor becomes dead after the redirect and its
    // contributing-asm history needs to be absorbed by the inner-cond
    // node (the new If consumer).  When the Xor has other live uses,
    // those uses still produce the value via its own fingerprint, so
    // transferring would CONTAMINATE inner_node's fingerprint with
    // addresses that don't contribute to its value (false positives
    // violate the contract that a fingerprint names the asm insns
    // whose lifting or rewrite contributed to that node's value).
    let xor_uses_before = function.output_uses(cond_out).count();
    function.update_input(cond_input_id, inner);
    if xor_uses_before == 1 {
        let inner_node = function.node_for_output(inner);
        function.extend_asm_fingerprint_from(inner_node, xor_node);
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
