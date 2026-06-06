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
//! into the canonical direct shape so [`strider_pattern::IfPat`] only
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
//! Add to `default_pipeline` after `ConstantFold` so any chained
//! `Xor(_, 1)` simplification has already collapsed before we look for
//! the canonical shape.  Without that ordering, the doubly-inverted form
//! would land in canonical form via two applications instead of one —
//! still correct, just one extra fixed-point iteration.
//!
//! ## Why this is a dedicated pass and not a `crate::rewrite_rule`
//!
//! The `crate::rewrite_rule` engine doesn't currently support rewrites
//! that swap consumers across two of a node's outputs — its model is
//! "find a matching subtree, replace its single output's consumers with
//! a fresh node's output."  The cond-inversion rewrite needs:
//!   - input redirection (cond slot 1 → inner of Xor);
//!   - bidirectional consumer swap on the two `Control` outputs.
//!
//! Both are use-list mutations the pattern-rewrite engine doesn't do, so
//! we hand-write the surgery.

use std::rc::Rc;

use strider_ir::IRViewer;
use strider_ir::node::{NodeId, NodeKind, ValueId};

use crate::error::Result;
use crate::peephole::PeepholeRewrite;
use strider_pattern::{Capture, MatchPat, Matcher, Pattern, bool_not, var};

/// Pass that rewrites `If(Xor(C, IntConst(1)):I1)` into `If(C)` with branches
/// swapped.
///
/// Add to `default_pipeline` after `ConstantFold` so chained
/// `Xor(_, 1)` reassoc simplifies double-negations first.
///
/// The inner `bool_not(var(x))` pattern is built once by
/// [`IfCondInversion::new`].  A built [`Pattern`] is `!Send + !Sync` and
/// not `Clone`, so it is held behind an [`Rc`] to keep the pass cheaply
/// `Clone` (cloning the pass shares the same pattern); the `Capture` slot
/// is `Copy`.
#[derive(Clone)]
pub struct IfCondInversion {
    inner_pat: Rc<Pattern>,
    inner_capture: Capture,
}

impl IfCondInversion {
    /// Builds the inner logical-NOT pattern once and returns a pass that
    /// owns it.
    pub fn new() -> Self {
        let inner_capture = Capture::new();
        Self {
            inner_pat: Rc::new(bool_not(var(inner_capture)).into_pattern()),
            inner_capture,
        }
    }
}

impl Default for IfCondInversion {
    fn default() -> Self {
        Self::new()
    }
}

impl crate::peephole::PeepholePass for IfCondInversion {
    fn matches_kind(&self, kind: &NodeKind) -> bool {
        matches!(kind, NodeKind::If)
    }

    fn try_rewrite(
        &self,
        ctx: &mut crate::EditFunction<'_>,
        root: NodeId,
    ) -> Result<PeepholeRewrite> {
        let Some(inner_value) =
            is_inverted_cond_match(ctx.function(), root, &self.inner_pat, self.inner_capture)
        else {
            return Ok(PeepholeRewrite::NoChange);
        };
        // `invert` only redirects the cond input and swaps the If's
        // existing true/false control consumers — no fresh node is built,
        // so report `new_node: None`.
        invert(ctx, root, inner_value)?;
        Ok(PeepholeRewrite::Changed { new_node: None })
    }

    /// Inverting an `If` swaps its control consumers but doesn't fold
    /// into a constant — re-enqueueing consumers would only re-walk
    /// joins that haven't changed shape.
    fn propagate_to_consumers(&self) -> bool {
        false
    }
}

/// Returns `Some(inner_out)` when the `If` node's cond input is the
/// canonical 1-bit logical NOT shape — an `Xor(x, IntConst(1)):I1` — as
/// matched by the `bool_not(var(x))` pattern owned by the pass.  The
/// bound capture is the Xor's non-constant operand `x`, which the caller
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
    inner_pat: &Pattern,
    inner_capture: Capture,
) -> Option<ValueId> {
    let [_ctrl, cond_value] = function.graph().node_inputs_exact::<2>(if_node).ok()?;
    let cond_node = function.producer(cond_value);
    // `match_at` is the single-node entry point: try the pattern at
    // exactly the cond's producer node (not a full graph walk).
    let m = Matcher::try_new(function).ok()?;
    let hit = m
        .match_at(cond_node, inner_pat)
        .expect("classifier pattern is single-rooted")?;
    hit.value(inner_capture)
}

/// Performs the inversion in place:
///   1. Re-points the `If`'s cond input from the `Xor(X, 1)` output to `X`.
///   2. Swaps the consumers of the two control outputs.
fn invert(
    ctx: &mut crate::EditFunction<'_>,
    if_node: NodeId,
    inner: strider_ir::node::ValueId,
) -> Result<()> {
    // Redirect cond input from the `Xor(X, 1)` output to `X`.
    //
    // After this step the Xor is unreferenced from the If; its other
    // consumers (if any) keep using it, which is fine.
    //
    // `EditFunction::redirect_input` rewires the one input edge and, when
    // this redirect leaves the Xor dead (it was the Xor's only use),
    // absorbs the Xor's contributing-asm history into the inner-cond
    // node (the new If consumer) — exactly the conditional absorption
    // this inversion needs.  When the Xor keeps other live uses, no
    // absorption happens, so `inner`'s fingerprint is never contaminated
    // with addresses that don't contribute to its value.
    let cond_use_id = ctx.graph_ref().node_input_id_at(if_node, 1)?;
    ctx.redirect_input(cond_use_id, inner);

    // Swap consumers between output[0] (true) and output[1] (false).
    //
    // Both outputs share the same producer node (`if_node`), and each output
    // has its own use-list.  `value_uses` yields `(consumer_node, input_idx)`
    // pairs; resolve each to a stable `UseId` before mutating, since
    // `update_input` rewrites the use-list and would invalidate any
    // half-consumed iterator.  Collect both lists before any redirect.
    let [true_value, false_value] = ctx.node_outputs_exact::<2>(if_node)?;
    let true_use_ids: smallvec::SmallVec<[strider_ir::node::UseId; 4]> = ctx
        .graph_ref()
        .value_uses(true_value)
        .map(|(consumer, idx)| ctx.graph_ref().node_input_id_at(consumer, idx as usize))
        .collect::<Result<_>>()?;
    let false_use_ids: smallvec::SmallVec<[strider_ir::node::UseId; 4]> = ctx
        .graph_ref()
        .value_uses(false_value)
        .map(|(consumer, idx)| ctx.graph_ref().node_input_id_at(consumer, idx as usize))
        .collect::<Result<_>>()?;
    for use_id in true_use_ids {
        ctx.update_input(use_id, false_value);
    }
    for use_id in false_use_ids {
        ctx.update_input(use_id, true_value);
    }
    Ok(())
}

#[cfg(test)]
mod tests;
