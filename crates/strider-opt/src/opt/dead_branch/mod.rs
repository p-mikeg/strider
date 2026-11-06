//! Folds an `If` with a constant condition or a `Switch` with a constant
//! dispatch address: the live control output is replaced with the branch's
//! control input, and the branch (side-effecting, so never culled) is killed.
//!
//! A fold leaves joins with dead predecessor slots; every run strips the
//! slots entry no longer reaches, with their phi inputs.

use entity_utils::DenseEntitySet;
use strider_ir::node::{NodeId, NodeKind, ValueId};
use strider_ir::walk::cfg_reachable;
use strider_ir::{IRViewer, IRWalker};

use crate::Result;
use crate::peephole::PeepholePass;

#[cfg(test)]
mod strip_tests;
#[cfg(test)]
mod tests;

pub struct DeadBranchElimination;

impl PeepholePass for DeadBranchElimination {
    type Sweep = EscapeOracle;

    fn matches_kind(&self, kind: &NodeKind) -> bool {
        matches!(kind, NodeKind::If | NodeKind::Switch(_))
    }

    fn try_rewrite(
        &self,
        escapes: &mut EscapeOracle,
        edit: &mut crate::EditFunction<'_>,
        _opt_ctx: &crate::pipeline::OptCtx<'_>,
        root: NodeId,
    ) -> Result<Option<ValueId>> {
        let Some(selection) = constant_selection(edit, root) else {
            return Ok(None);
        };
        if !escapes.live_side_escapes(edit, selection.live) {
            return Ok(None);
        }
        // The selector is the proof for this arm; keep its fingerprint.
        let ctrl_value = edit.node_inputs(root)[NodeKind::CTRL_SLOT];
        edit.absorb_fingerprint(ctrl_value, selection.selector);
        edit.replace_value(selection.live, ctrl_value);
        edit.kill_node(root);
        Ok(None)
    }

    fn after_sweep(&self, edit: &mut crate::EditFunction<'_>, changed: bool) -> Result<()> {
        // A fold detaches its dead arm from control, which the incremental
        // bookkeeping does not track.
        if changed {
            edit.clean();
        }
        if strip_dead_predecessors(edit) || changed {
            edit.resync_live_set();
        }
        Ok(())
    }
}

/// Strips every `Region` predecessor slot whose producer is unreachable from
/// entry, with the matching `Phi` / `MemPhi` slots; `true` if it stripped
/// one. The caller resyncs the live set.
fn strip_dead_predecessors(edit: &mut crate::EditFunction<'_>) -> bool {
    let entry = edit.entry();
    // Live, not control-reachable, joins: a join reached only through data
    // can hold a dead slot.
    let reachable = cfg_reachable(edit.function().graph(), entry);

    let regions: Vec<NodeId> = edit
        .live_reverse_postorder_filter(|k| matches!(k, NodeKind::Region))
        .collect();
    let mut stripped = false;
    for region in regions {
        let dead: Vec<u32> = edit
            .node_inputs(region)
            .into_iter()
            .enumerate()
            .filter(|&(_, input)| !reachable.contains(edit.producer(input)))
            .map(|(idx, _)| idx as u32)
            .collect();
        if !dead.is_empty() {
            edit.remove_region_predecessors(region, &dead);
            stripped = true;
        }
    }
    stripped
}

/// The control output a constant-selector branch takes.
struct Selection {
    /// The constant the branch tests.
    selector: ValueId,
    live: ValueId,
}

/// The selection of an `If` or `Switch` with a constant selector; `None`
/// otherwise, or for a `Switch` whose constant no arm lists.
fn constant_selection(edit: &crate::EditFunction<'_>, node: NodeId) -> Option<Selection> {
    let (selector, live) = match edit.node_kind(node) {
        NodeKind::If => {
            let cond = edit.if_cond(node);
            (cond, usize::from(!edit.bool_const_val(cond)?))
        }
        NodeKind::Switch(_) => {
            let selector = edit.switch_selector(node);
            let k = edit.int_const_u128(selector)?;
            let live = edit
                .function()
                .switch_targets(node)
                .iter()
                .position(|&t| u128::from(t) == k)?;
            (selector, live)
        }
        _ => return None,
    };
    Some(Selection {
        selector,
        live: edit.node_outputs(node)[live],
    })
}

#[cfg(test)]
thread_local! {
    /// [`escaping_nodes`] builds.
    static ESCAPE_SETS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Whether a constant branch's surviving arm still reaches a terminator once
/// folded, from one [`escaping_nodes`] set per sweep.
#[derive(Default)]
pub(crate) struct EscapeOracle {
    escapes: Option<DenseEntitySet<NodeId>>,
}

impl EscapeOracle {
    /// Whether the surviving successor reaches a terminator without taking the
    /// dead arm of any constant branch; folding otherwise would leave an
    /// exit-free cycle, which [`strider_ir::validate`] rejects. A dangling
    /// control output other than a dead arm counts as an escape.
    fn live_side_escapes(&mut self, edit: &crate::EditFunction<'_>, live_ctrl: ValueId) -> bool {
        let mut succs = edit.value_uses(live_ctrl).map(|(node, _)| node).peekable();
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
/// Valid for the whole sweep: a fold changes no route this set was built from.
fn escaping_nodes(edit: &crate::EditFunction<'_>) -> DenseEntitySet<NodeId> {
    #[cfg(test)]
    ESCAPE_SETS.with(|c| c.set(c.get() + 1));
    let mut dead_arms: DenseEntitySet<ValueId> = DenseEntitySet::new();
    let nodes: Vec<NodeId> = edit.walk().collect();
    for &node in &nodes {
        if let Some(selection) = constant_selection(edit, node) {
            for &arm in edit.node_outputs(node) {
                if arm != selection.live {
                    dead_arms.insert(arm);
                }
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
