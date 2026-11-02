//! Whether a constant branch's surviving arm still reaches a terminator once
//! the branch is folded, answered for every root of one sweep.

use entity_utils::DenseEntitySet;
use strider_ir::node::{NodeId, ValueId};
use strider_ir::{IRViewer, IRWalker};

use super::constant_selection;

#[cfg(test)]
thread_local! {
    /// [`escaping_nodes`] builds, which a test zeroes before measuring.
    pub(super) static ESCAPE_SETS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// [`escaping_nodes`], built by the first root of a sweep that asks.
#[derive(Default)]
pub(crate) struct EscapeOracle {
    escapes: Option<DenseEntitySet<NodeId>>,
}

impl EscapeOracle {
    /// Does the surviving successor reach a terminator without taking the
    /// dead arm of any constant branch?
    ///
    /// That is the fold's condition. A loop whose only exits are dead arms
    /// never runs out of it, and folding any of those branches would leave an
    /// exit-free cycle, which [`strider_ir::validate`] rejects as
    /// `NoTerminatorReachable`; no branch inside such a loop folds.
    ///
    /// Mirrors the validator: a dangling control output other than a dead arm
    /// counts as an escape, so a half-wired CFG mid-pipeline is not read as a
    /// stranded one.
    pub(super) fn live_side_escapes(
        &mut self,
        edit: &crate::EditFunction<'_>,
        live_ctrl: ValueId,
    ) -> bool {
        let mut succs = edit.value_uses(live_ctrl).map(|(node, _)| node).peekable();
        // The surviving arm having no consumer at all is a dangling edge, not
        // a stranded one.
        if succs.peek().is_none() {
            return true;
        }
        let escapes = self.escapes.get_or_insert_with(|| escaping_nodes(edit));
        succs.any(|node| escapes.contains(node))
    }
}

/// Nodes from which a terminator or a dangling control edge is reachable
/// without taking the dead arm of a constant-selector branch.
///
/// Valid for the whole sweep.  A fold rewires the live arm onto the branch's
/// own control input and orphans the dead cone, neither of which is on any
/// route this set was built from.
fn escaping_nodes(edit: &crate::EditFunction<'_>) -> DenseEntitySet<NodeId> {
    #[cfg(test)]
    ESCAPE_SETS.with(|c| c.set(c.get() + 1));
    let mut dead_arms: DenseEntitySet<ValueId> = DenseEntitySet::new();
    let nodes: Vec<NodeId> = edit.walk().collect();
    for &node in &nodes {
        if let Some(selection) = constant_selection(edit, node) {
            for arm in selection.dead {
                dead_arms.insert(arm);
            }
        }
    }

    let live_ctrl_outputs = |node: NodeId| {
        strider_ir::walk::cfg_outputs(edit.function().graph(), node)
            .filter(|&out| !dead_arms.contains(out))
    };

    let mut escapes: DenseEntitySet<NodeId> = DenseEntitySet::new();
    let mut stack: Vec<NodeId> = Vec::new();
    for &node in &nodes {
        let escapes_here = edit.node_kind(node).is_terminator()
            || live_ctrl_outputs(node).any(|out| edit.value_uses(out).next().is_none());
        if escapes_here && escapes.insert(node) {
            stack.push(node);
        }
    }

    strider_ir::walk::close_over_control_preds(
        edit.function().graph(),
        &mut escapes,
        stack,
        |v| dead_arms.contains(v),
        None,
    );
    escapes
}
