//! Folds a control branch with a constant selector: an `If` with a constant
//! `I1` condition, or a `Switch` with a constant dispatch address.  The live
//! control output is replaced with the branch's own control input so the live
//! successor is wired past it, then the branch node is killed unconditionally
//! (it is side-effecting, so the automatic dead-cone cull never reaches it).
//!
//! The fold is DECLINED where dropping the dead arm would strand memory: an
//! `Unreachable` consuming the dead control output, which only the `If` arm
//! tests, or a live side reaching no terminator, which both test.
//!
//! A dead subgraph can still escape to live data (a dead `Call`'s `mem_value`
//! flowing into a live `MemPhi`), leaving it transiently reachable backward
//! through data.  Validation runs only once the destructive pipeline converges,
//! so that transient shape is never observed.
//!
//! Dead predecessor slots of the joins are stripped in the same pass.

use strider_ir::IRViewer;
use strider_ir::node::{NodeId, NodeKind};

use crate::error::Result;
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
        ctx: &mut OptCtx<'_>,
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
        _opt_ctx: &mut crate::pipeline::OptCtx<'_>,
        root: NodeId,
    ) -> Result<PeepholeRewrite> {
        match edit.node_kind(root) {
            NodeKind::If => {
                // Inputs are [ctrl_in, condition].  A detached If is never handed
                // back here (no consumer re-enqueue, no reported `new_node`), so
                // `root` always still carries both.
                let [ctrl_value, cond_value] = edit
                    .graph_ref()
                    .node_inputs_exact::<2>(root)
                    .expect("If has 2 inputs per node signature");

                let Some(cond_val) = edit.function().bool_const_val(cond_value) else {
                    return Ok(PeepholeRewrite::NoChange);
                };

                let [ctrl_true, ctrl_false] = edit
                    .node_outputs_exact::<2>(root)
                    .expect("If has 2 outputs per node signature");
                let live_ctrl = if cond_val { ctrl_true } else { ctrl_false };
                let dead_ctrl = if cond_val { ctrl_false } else { ctrl_true };

                // An `Unreachable` on the dead arm anchors the memory of an
                // exit-free control cycle the fold would orphan.  Coarser than
                // `live_side_escapes` below: a dead arm that is a bare
                // `Unreachable` declines too, though folding it is sound.  The
                // asymmetry is the point: declining costs one unfolded branch,
                // folding wrongly costs a store.  `if (const) .. else abort();`
                // still folds, its dead arm reaching the `Unreachable` through
                // the `Call`.
                let dead_consumers: Vec<NodeId> = edit
                    .graph_ref()
                    .value_uses(dead_ctrl)
                    .map(|(node, _)| node)
                    .collect();
                if dead_consumers
                    .iter()
                    .any(|&node| matches!(edit.node_kind(node), NodeKind::Unreachable))
                {
                    return Ok(PeepholeRewrite::NoChange);
                }

                if !escapes.live_side_escapes(edit, root, live_ctrl) {
                    return Ok(PeepholeRewrite::NoChange);
                }

                // The condition is the proof for taking this arm, so its
                // fingerprint must survive `kill_node` cascade-culling the
                // condition cone.  Absorb it into the surviving control source.
                edit.absorb_fingerprint(ctrl_value, cond_value);

                edit.replace_value(live_ctrl, ctrl_value);
                edit.kill_node(root);
                Ok(PeepholeRewrite::Changed { new_node: None })
            }
            NodeKind::Switch(_) => {
                let inputs = edit.graph_ref().node_inputs(root);
                let (ctrl_value, addr_value) = (inputs[0], inputs[1]);

                let Some(k) = edit.function().int_const_u128(addr_value) else {
                    return Ok(PeepholeRewrite::NoChange);
                };

                // Output i corresponds to cases[i].  Computing the index inside
                // this expression ends the immutable borrow of `edit.function()`
                // before the mutable calls below, without cloning the slice.
                let Some(i) = edit
                    .function()
                    .switch_targets(root)
                    .iter()
                    .position(|&t| u128::from(t) == k)
                else {
                    // Reachable: a `Switch`'s arms "need not cover every
                    // dispatch value" (`node_signature.rs`), a site the
                    // resolver seated early holding one arm and widening in a
                    // later round.
                    return Ok(PeepholeRewrite::NoChange);
                };
                let live_ctrl = edit.node_outputs(root)[i];

                if !escapes.live_side_escapes(edit, root, live_ctrl) {
                    return Ok(PeepholeRewrite::NoChange);
                }

                // Same proof-completeness rationale as the `If` arm, with the
                // constant dispatch address in place of the condition.
                edit.absorb_fingerprint(ctrl_value, addr_value);
                edit.replace_value(live_ctrl, ctrl_value);
                edit.kill_node(root);
                Ok(PeepholeRewrite::Changed { new_node: None })
            }
            _ => Ok(PeepholeRewrite::NoChange),
        }
    }
}
