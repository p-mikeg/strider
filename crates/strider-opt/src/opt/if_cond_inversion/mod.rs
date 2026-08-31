//! Rewrites `If(Xor(C, IntConst(1))) {A} {B}` into `If(C) {B} {A}`.
//!
//! Convergence: each application removes one `Xor`-with-1 from the cond, and a
//! doubly-inverted cond collapses first via `ConstantFold`'s `!!x -> x`.

use strider_ir::IRViewer;
use strider_ir::node::{NodeId, NodeKind, ValueId};

use crate::error::Result;
use crate::peephole::PeepholeRewrite;
use strider_pattern::{Capture, MatchPat, Matcher, Pattern, bool_not, var};

#[derive(Clone)]
pub struct IfCondInversion;

thread_local! {
    /// Built once per thread, and held here rather than in the pass so the
    /// pass stays `Send`; see `ConstantFold`.
    static PATTERN: (Pattern, Capture) = {
        let capture = Capture::new();
        (bool_not(var(capture)).into_pattern(), capture)
    };
}

impl IfCondInversion {
    #[must_use]
    pub fn new() -> Self {
        Self
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
            PATTERN.with(|(pat, cap)| is_inverted_cond_match(edit.function(), root, pat, *cap))
        else {
            return Ok(PeepholeRewrite::NoChange);
        };
        // `invert` builds no fresh node, hence `new_node: None`.
        invert(edit, root, inner_value)?;
        Ok(PeepholeRewrite::Changed { new_node: None })
    }

    fn propagate_to_consumers(&self) -> bool {
        false
    }
}

/// Returns the `Xor`'s non-constant operand when the `If` cond is the canonical
/// 1-bit logical NOT `Xor(x, IntConst(1)):I1`.
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
