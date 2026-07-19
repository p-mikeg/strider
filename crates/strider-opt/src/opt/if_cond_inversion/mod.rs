//! Rewrites `If(Xor(C, IntConst(1))) {A} {B}` into `If(C) {B} {A}`, so pattern
//! matchers only ever see a non-inverted `If` condition.  Lifters emit either
//! sense depending on the arch's flag-test instruction; this collapses both to
//! one shape.
//!
//! Convergence: each application removes one `Xor`-with-1 from the cond, and a
//! doubly-inverted cond collapses first via `ConstantFold`'s xor-reassoc.
//!
//! Run after `ConstantFold` so chained `Xor(_, 1)` has already simplified;
//! without that ordering a double inversion just takes two iterations.
//!
//! Hand-written rather than a `crate::rewrite_rule`: the rewrite engine
//! replaces one output's consumers with a fresh node, but this needs input
//! redirection plus a bidirectional consumer swap across two `Control` outputs.

use std::rc::Rc;

use strider_ir::IRViewer;
use strider_ir::node::{NodeId, NodeKind, ValueId};

use crate::error::Result;
use crate::peephole::PeepholeRewrite;
use strider_pattern::{Capture, MatchPat, Matcher, Pattern, bool_not, var};

/// A built [`Pattern`] is not `Clone`, so it is held behind an [`Rc`] to keep
/// the pass cheaply `Clone`; clones share the same pattern.
#[derive(Clone)]
pub struct IfCondInversion {
    inner_pat: Rc<Pattern>,
    inner_capture: Capture,
}

impl IfCondInversion {
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
        edit: &mut crate::EditFunction<'_>,
        _opt_ctx: &mut crate::pipeline::OptCtx<'_>,
        root: NodeId,
    ) -> Result<PeepholeRewrite> {
        let Some(inner_value) =
            is_inverted_cond_match(edit.function(), root, &self.inner_pat, self.inner_capture)
        else {
            return Ok(PeepholeRewrite::NoChange);
        };
        // `invert` builds no fresh node, hence `new_node: None`.
        invert(edit, root, inner_value)?;
        Ok(PeepholeRewrite::Changed { new_node: None })
    }

    /// Inversion swaps control consumers without folding to a constant, so
    /// re-enqueueing consumers would only re-walk unchanged joins.
    fn propagate_to_consumers(&self) -> bool {
        false
    }
}

/// Returns the `Xor`'s non-constant operand when the `If` cond is the canonical
/// 1-bit logical NOT `Xor(x, IntConst(1)):I1`.
///
/// Goes through the matcher rather than a hand-rolled check so the LHS shape
/// (commutative Xor with the I1 all-ones constant, plus the I1-output guard)
/// stays owned by the `bool_not` builder.
fn is_inverted_cond_match(
    function: &strider_ir::Function,
    if_node: NodeId,
    inner_pat: &Pattern,
    inner_capture: Capture,
) -> Option<ValueId> {
    let cond_value = function.if_cond(if_node);
    let cond_node = function.producer(cond_value);
    // `match_at` tries the pattern at exactly this node, no graph walk.
    let m = Matcher::new(function);
    let hit = m
        .match_at(cond_node, inner_pat)
        .expect("classifier pattern is single-rooted")?;
    hit.value(inner_capture)
}

/// Resolves every consuming edge to a stable `UseId` up front, so a later
/// `update_input` can't invalidate a half-consumed use-list iterator.
fn input_use_ids(
    edit: &crate::EditFunction<'_>,
    value: strider_ir::node::ValueId,
) -> Result<smallvec::SmallVec<[strider_ir::node::UseId; 4]>> {
    edit.value_uses(value)
        .map(|(consumer, idx)| edit.node_input_id_at(consumer, idx as usize))
        .collect()
}

fn invert(
    edit: &mut crate::EditFunction<'_>,
    if_node: NodeId,
    inner: strider_ir::node::ValueId,
) -> Result<()> {
    // Redirect the cond input from the `Xor(X, 1)` output to `X`.
    //
    // `redirect_input` absorbs the Xor's asm history into `inner` only when
    // this redirect leaves the Xor dead.  If the Xor keeps other live uses it
    // still computes its own value, so absorbing would be false attribution.
    let cond_use_id = edit.node_input_id_at(if_node, 1)?;
    edit.redirect_input(cond_use_id, inner);

    // Swap consumers between output[0] (true) and output[1] (false).  Both
    // use-lists must be collected before any redirect: `update_input` rewrites
    // them in place.
    let [true_value, false_value] = edit.node_outputs_exact::<2>(if_node)?;
    let true_use_ids = input_use_ids(edit, true_value)?;
    let false_use_ids = input_use_ids(edit, false_value)?;
    for use_id in true_use_ids {
        edit.update_input(use_id, false_value);
    }
    for use_id in false_use_ids {
        edit.update_input(use_id, true_value);
    }
    Ok(())
}

#[cfg(test)]
mod tests;
