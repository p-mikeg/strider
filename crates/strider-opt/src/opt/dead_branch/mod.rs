//! Folds a control branch with a constant selector: an `If` with a constant
//! `I1` condition, or a `Switch` with a constant dispatch address.  The live
//! control output is replaced with the branch's own control input so the live
//! successor is wired past it, then the branch node is killed unconditionally
//! (it is side-effecting, so the automatic dead-cone cull never reaches it).
//!
//! A branch folds only when its live arm reaches a terminator without taking
//! the dead arm of any constant branch. So no branch folds whose live arm
//! enters a loop that only a dead arm leaves, the loop's own branches and
//! one before it alike.
//!
//! A dead subgraph can still escape to live data (a dead `Call`'s `mem_value`
//! flowing into a live `MemPhi`), leaving it transiently reachable backward
//! through data.  Validation runs only once the destructive pipeline converges,
//! so that transient shape is never observed.
//!
//! Every run then strips each join's dead predecessor slots, whichever pass
//! or lift left them dead.

use entity_utils::DenseEntitySet;
use smallvec::SmallVec;
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
        // The constant selector is the proof for taking this arm, so its
        // fingerprint must survive `kill_node` cascade-culling its cone.
        let ctrl_value = edit.node_inputs(root)[NodeKind::CTRL_SLOT];
        edit.absorb_fingerprint(ctrl_value, selection.selector);
        edit.replace_value(selection.live, ctrl_value);
        edit.kill_node(root);
        Ok(None)
    }

    fn after_sweep(&self, edit: &mut crate::EditFunction<'_>, changed: bool) -> Result<()> {
        // The pipeline drains between passes; do the same before the strip.
        // A fold detaches its dead arm from control, which the incremental
        // bookkeeping does not track, so resync even when nothing is stripped.
        if changed {
            edit.clean();
        }
        if strip_dead_predecessors(edit) || changed {
            edit.resync_live_set();
        }
        Ok(())
    }
}

/// Strips every `Region` predecessor slot whose control producer entry no
/// longer reaches, with the matching `Phi` / `MemPhi` slots; `true` if it
/// stripped one.
///
/// The incremental live-set bookkeeping tracks data orphaning only, so the
/// caller resyncs after a strip: a severed slot can detach a whole subgraph,
/// and a `MemPhi` left cached-live with zero arms trips the memory-SSA walk.
fn strip_dead_predecessors(edit: &mut crate::EditFunction<'_>) -> bool {
    let entry = edit.entry();
    // Two different reachability notions here. A predecessor edge is dead
    // iff its control producer is control-unreachable from entry, but the
    // set we ITERATE is the general graph walk: a join that is only
    // data-reachable (an escaping dead branch whose value still feeds a live
    // phi) is still validator-visible and can carry a dead control slot.
    let reachable = cfg_reachable(edit.function().graph(), entry);

    // The cached live set covers the same nodes as a fresh walk, so reuse
    // its RPO rather than re-walking.
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
        // Each region's dead indices go at once: batch removal is set-based,
        // and a strip touches only its own region, so the order commutes.
        if !dead.is_empty() {
            edit.remove_region_predecessors(region, &dead);
            stripped = true;
        }
    }
    stripped
}

/// The control output a constant-selector branch takes, and the ones it
/// never takes.
struct Selection {
    /// The constant the branch tests.
    selector: ValueId,
    live: ValueId,
    dead: SmallVec<[ValueId; 2]>,
}

/// `None` for anything but an `If` or `Switch` with a constant selector, and
/// for a `Switch` whose constant no arm lists: its arms need not cover every
/// dispatch value (`node/signature.rs`), as at a site the resolver seated
/// early holding one arm.  The fold and [`EscapeOracle`] both read this, so
/// they agree on which arms are dead.
fn constant_selection(edit: &crate::EditFunction<'_>, node: NodeId) -> Option<Selection> {
    let (selector, live) = match edit.node_kind(node) {
        NodeKind::If => {
            // Outputs are [true, false].
            let cond = edit.if_cond(node);
            (cond, usize::from(!edit.bool_const_val(cond)?))
        }
        NodeKind::Switch(_) => {
            // Output i is cases[i].
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
    let outputs = edit.node_outputs(node);
    Some(Selection {
        selector,
        live: outputs[live],
        dead: outputs
            .iter()
            .enumerate()
            .filter(|&(i, _)| i != live)
            .map(|(_, &out)| out)
            .collect(),
    })
}

#[cfg(test)]
thread_local! {
    /// [`escaping_nodes`] builds, which a test zeroes before measuring.
    static ESCAPE_SETS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Whether a constant branch's surviving arm still reaches a terminator once
/// the branch is folded, answered for every root of one sweep from
/// [`escaping_nodes`], built by the first root that asks.
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
    fn live_side_escapes(&mut self, edit: &crate::EditFunction<'_>, live_ctrl: ValueId) -> bool {
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
