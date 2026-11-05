//! Rewrites `If(Xor(C, IntConst(1))) {A} {B}` into `If(C) {B} {A}`.

use strider_ir::IRViewer;
use strider_ir::node::{NodeId, NodeKind, UseId, ValueId};

use crate::Result;
use strider_pattern::{Capture, MatchPat, Matcher, Pattern, bool_not, var};

pub struct IfCondInversion;

thread_local! {
    /// Outside the pass so it stays `Sync`.
    static PATTERN: (Pattern, Capture) = {
        let capture = Capture::new();
        (bool_not(var(capture)).into_pattern(), capture)
    };
}

impl crate::peephole::PeepholePass for IfCondInversion {
    type Sweep = ();

    fn matches_kind(&self, kind: &NodeKind) -> bool {
        matches!(kind, NodeKind::If)
    }

    fn try_rewrite(
        &self,
        _sweep: &mut (),
        edit: &mut crate::EditFunction<'_>,
        _opt_ctx: &crate::pipeline::OptCtx<'_>,
        root: NodeId,
    ) -> Result<Option<ValueId>> {
        if let Some(inner_value) = bool_not_operand(edit.function(), edit.function().if_cond(root))
        {
            invert(edit, root, inner_value)?;
        }
        Ok(None)
    }
}

/// `x` when `value` is the canonical 1-bit logical NOT `Xor(x, IntConst(1)):I1`.
pub(crate) fn bool_not_operand(function: &strider_ir::Function, value: ValueId) -> Option<ValueId> {
    PATTERN.with(|(pat, cap)| {
        Matcher::new(function)
            .match_at(function.producer(value), pat)
            .expect("classifier pattern is single-rooted")?
            .value(*cap)
    })
}

/// Every consuming edge of `value` as a stable `UseId`, collected before any
/// `update_input` rewrites the use-list.
fn input_use_ids(
    edit: &crate::EditFunction<'_>,
    value: ValueId,
) -> Result<smallvec::SmallVec<[UseId; 4]>> {
    edit.value_uses(value)
        .map(|(consumer, idx)| edit.node_input_id_at(consumer, idx as usize))
        .collect()
}

fn invert(edit: &mut crate::EditFunction<'_>, if_node: NodeId, inner: ValueId) -> Result<()> {
    // `redirect_input` absorbs the Xor's fingerprint only when this leaves it
    // dead; a still-live Xor computes its own value.
    let cond_use_id = edit.node_input_id_at(if_node, NodeKind::IF_COND_SLOT)?;
    edit.redirect_input(cond_use_id, inner);

    // Swap the true and false consumers; both use-lists are collected first.
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
