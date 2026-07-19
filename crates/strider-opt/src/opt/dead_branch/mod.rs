//! Folds a control branch with a constant selector: an `If` with a constant
//! `I1` condition, or a `Switch` with a constant dispatch address.  The live
//! control output is replaced with the branch's own control input so the live
//! successor is wired past it, then the branch node is killed unconditionally
//! (it is side-effecting, so the automatic dead-cone cull never reaches it).
//!
//! A dead subgraph can still escape to live data (a dead `Call`'s `mem_value`
//! flowing into a live `MemPhi`), which leaves it transiently reachable
//! backward through data.  [`crate::CfgDetach`] severs the dead
//! `Region`-predecessor edge and [`crate::PhiCollapse`] finishes the teardown;
//! validation runs only once the destructive pipeline converges, so the
//! transient shape is never observed.  Nodes left fully unreachable stay in the
//! arena; the validator and pattern queries only walk from entry.
//!
//! Stripping `Region` predecessor slots is [`crate::CfgDetach`]'s job, not this
//! pass's.

use strider_ir::IRViewer;
use strider_ir::node::{NodeId, NodeKind};

use crate::error::Result;
use crate::peephole::{PeepholePass, PeepholeRewrite};

#[cfg(test)]
mod tests;

/// Runs with [`crate::CfgDetach`] (removes the dead `Region` predecessor slot)
/// and [`crate::PhiCollapse`] / [`crate::RegionCollapse`] (collapse the
/// now-single-pred join).
#[derive(Clone, Copy)]
pub struct DeadBranchElimination;

impl PeepholePass for DeadBranchElimination {
    fn matches_kind(&self, kind: &NodeKind) -> bool {
        matches!(kind, NodeKind::If | NodeKind::Switch)
    }

    fn try_rewrite(
        &self,
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

                // The condition is the proof for taking this arm unconditionally,
                // so its fingerprint must survive `kill_node` cascade-culling the
                // condition cone.  Absorb it into the surviving control source.
                // Over-tainting is fine; the fingerprint is a superset proof aid,
                // not a minimal determining set.
                edit.absorb_fingerprint(ctrl_value, cond_value);

                // `kill_node` also enqueues the now-dead pure operands for `clean`
                // to cascade-cull.  Redirecting to an existing edge creates no
                // fresh node, hence `new_node: None`.
                edit.replace_value(live_ctrl, ctrl_value)?;
                edit.kill_node(root);
                Ok(PeepholeRewrite::Changed { new_node: None })
            }
            NodeKind::Switch => {
                let [ctrl_value, addr_value] = edit
                    .graph_ref()
                    .node_inputs_exact::<2>(root)
                    .expect("Switch has 2 inputs per node signature");

                let Some(k) = edit.function().int_const_u128(addr_value) else {
                    return Ok(PeepholeRewrite::NoChange);
                };

                // Output i corresponds to cases[i].  Computing the index inside
                // this expression ends the immutable borrow of `edit.function()`
                // before the mutable calls below, without cloning the slice.
                let Some(i) = edit
                    .function()
                    .side_tables()
                    .switch_targets(root)
                    .iter()
                    .position(|&t| u128::from(t) == k)
                else {
                    return Ok(PeepholeRewrite::NoChange); // exhaustive table => shouldn't happen
                };
                let live_ctrl = edit.node_outputs(root)[i];

                // Same proof-completeness rationale as the `If` arm, with the
                // constant dispatch address in place of the condition.
                edit.absorb_fingerprint(ctrl_value, addr_value);
                edit.replace_value(live_ctrl, ctrl_value)?;
                edit.kill_node(root);
                Ok(PeepholeRewrite::Changed { new_node: None })
            }
            _ => Ok(PeepholeRewrite::NoChange),
        }
    }

    /// No value is folded into a constant here, so re-enqueueing consumers
    /// would only re-walk joins that `CfgDetach` cleans up separately.
    fn propagate_to_consumers(&self) -> bool {
        false
    }
}
