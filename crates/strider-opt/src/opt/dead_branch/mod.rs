//! Folds a control branch with a constant selector: an `If` with a constant
//! `I1` condition, or a `Switch` with a constant dispatch address.  The live
//! control output is replaced with the branch's own control input so the live
//! successor is wired past it, then the branch node is killed unconditionally
//! (it is side-effecting, so the automatic dead-cone cull never reaches it).
//!
//! The fold is DECLINED where dropping the dead arm would strand memory: an
//! `Unreachable` consuming the dead control output, which only an `If`
//! tests, or a live side reaching no terminator, which both test.
//!
//! A dead subgraph can still escape to live data (a dead `Call`'s `mem_value`
//! flowing into a live `MemPhi`), leaving it transiently reachable backward
//! through data.  Validation runs only once the destructive pipeline converges,
//! so that transient shape is never observed.
//!
//! Dead predecessor slots of the joins are stripped in the same pass.

use smallvec::SmallVec;
use strider_ir::IRViewer;
use strider_ir::node::{NodeId, NodeKind, ValueId};

use crate::Result;
use crate::opt::cfg_detach::strip_dead_predecessors;
use crate::peephole::{PeepholePass, PeepholeRewrite, run_peephole};
use crate::pipeline::{OptCtx, OptimizationResult, Optimizer};

mod escape;
#[cfg(test)]
mod tests;

use escape::EscapeOracle;
#[cfg(test)]
use escape::{FULL_WALKS, WALKED_NODES};

#[derive(Clone, Copy)]
pub struct DeadBranchElimination;

impl Optimizer for DeadBranchElimination {
    fn apply(
        &self,
        edit: &mut crate::EditFunction<'_>,
        ctx: &OptCtx<'_>,
    ) -> Result<OptimizationResult> {
        let folded = run_peephole(&DbeFold, edit, ctx)?;
        if folded.changed() {
            // The pipeline drains between passes; do the same before the strip.
            edit.clean();
            strip_dead_predecessors(edit)?;
        }
        Ok(folded)
    }
}

/// The branch fold itself. The blanket `Optimizer` impl over `PeepholePass`
/// forbids implementing both on [`DeadBranchElimination`].
#[derive(Clone, Copy)]
struct DbeFold;

impl PeepholePass for DbeFold {
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
    ) -> Result<PeepholeRewrite> {
        let Some(selection) = constant_selection(edit, root) else {
            return Ok(PeepholeRewrite::NoChange);
        };
        let root_is_if = matches!(edit.node_kind(root), NodeKind::If);
        // An `Unreachable` on the dead arm anchors the memory of an exit-free
        // control cycle the fold would orphan.  Coarser than
        // `live_side_escapes` below: a dead arm that is a bare `Unreachable`
        // declines too, though folding it is sound.  The asymmetry is the
        // point: declining costs one unfolded branch, folding wrongly costs a
        // store.  `if (const) .. else abort();` still folds, its dead arm
        // reaching the `Unreachable` through the `Call`.
        if root_is_if
            && selection.dead.iter().any(|&dead| {
                edit.value_uses(dead)
                    .any(|(node, _)| matches!(edit.node_kind(node), NodeKind::Unreachable))
            })
        {
            return Ok(PeepholeRewrite::NoChange);
        }
        if !escapes.live_side_escapes(edit, root, selection.live) {
            return Ok(PeepholeRewrite::NoChange);
        }
        // The constant selector is the proof for taking this arm, so its
        // fingerprint must survive `kill_node` cascade-culling its cone.
        let selector_slot = if root_is_if {
            NodeKind::IF_COND_SLOT
        } else {
            NodeKind::SWITCH_SELECTOR_SLOT
        };
        let (ctrl_value, selector) = {
            let inputs = edit.node_inputs(root);
            (inputs[NodeKind::CTRL_SLOT], inputs[selector_slot])
        };
        edit.absorb_fingerprint(ctrl_value, selector);
        edit.replace_value(selection.live, ctrl_value);
        edit.kill_node(root);
        Ok(PeepholeRewrite::Changed { new_node: None })
    }
}

/// The control output a constant-selector branch takes, and the ones it
/// never takes.
struct Selection {
    live: ValueId,
    dead: SmallVec<[ValueId; 2]>,
}

/// `None` for anything but an `If` or `Switch` with a constant selector, and
/// for a `Switch` whose constant no arm lists: its arms need not cover every
/// dispatch value (`node_signature.rs`), as at a site the resolver seated
/// early holding one arm.  The fold and [`EscapeOracle`] both read this, so
/// they agree on which arms are dead.
fn constant_selection(edit: &crate::EditFunction<'_>, node: NodeId) -> Option<Selection> {
    let live = match edit.node_kind(node) {
        NodeKind::If => {
            // Outputs are [true, false].
            let cond = edit.bool_const_val(edit.if_cond(node))?;
            usize::from(!cond)
        }
        NodeKind::Switch(_) => {
            // Output i is cases[i].
            let k = edit.int_const_u128(edit.switch_selector(node))?;
            edit.function()
                .switch_targets(node)
                .iter()
                .position(|&t| u128::from(t) == k)?
        }
        _ => return None,
    };
    let outputs = edit.node_outputs(node);
    Some(Selection {
        live: outputs[live],
        dead: outputs
            .iter()
            .enumerate()
            .filter(|&(i, _)| i != live)
            .map(|(_, &out)| out)
            .collect(),
    })
}
