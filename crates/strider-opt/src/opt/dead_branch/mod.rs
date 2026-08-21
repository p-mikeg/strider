//! Folds a control branch with a constant selector: an `If` with a constant
//! `I1` condition, or a `Switch` with a constant dispatch address.  The live
//! control output is replaced with the branch's own control input so the live
//! successor is wired past it, then the branch node is killed unconditionally
//! (it is side-effecting, so the automatic dead-cone cull never reaches it).
//!
//! A dead subgraph can still escape to live data (a dead `Call`'s `mem_value`
//! flowing into a live `MemPhi`), leaving it transiently reachable backward
//! through data.  Validation runs only once the destructive pipeline converges,
//! so that transient shape is never observed.

use entity_utils::DenseEntitySet;
use strider_ir::IRViewer;
use strider_ir::node::{NodeId, NodeKind, ValueId};

use crate::error::Result;
use crate::peephole::{PeepholePass, PeepholeRewrite};

#[cfg(test)]
mod tests;

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
                let dead_ctrl = if cond_val { ctrl_false } else { ctrl_true };

                // An `Unreachable` on the dead arm anchors the memory of an
                // exit-free control cycle. Folding the branch orphans it and the
                // cycle loses its stores.
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

                if !live_side_reaches_terminator(edit, root, live_ctrl) {
                    return Ok(PeepholeRewrite::NoChange);
                }

                // The condition is the proof for taking this arm, so its
                // fingerprint must survive `kill_node` cascade-culling the
                // condition cone.  Absorb it into the surviving control source.
                edit.absorb_fingerprint(ctrl_value, cond_value);

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

                if !live_side_reaches_terminator(edit, root, live_ctrl) {
                    return Ok(PeepholeRewrite::NoChange);
                }

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

    fn propagate_to_consumers(&self) -> bool {
        false
    }
}

/// Does the surviving successor still reach a terminator once `root` is gone?
///
/// A loop whose only exit is the dead arm becomes an exit-free cycle, which
/// [`strider_ir::validate`] rejects as `NoTerminatorReachable` and whose body
/// compaction then drops. An `Unreachable` directly on the dead arm is the
/// narrow case of this the guard above catches before the fold's other
/// bookkeeping; here the exit can sit any number of `Region`s away.
///
/// Mirrors the validator: a dangling control output counts as an escape, so a
/// half-wired CFG mid-pipeline is not read as a stranded one.
fn live_side_reaches_terminator(
    edit: &crate::EditFunction<'_>,
    root: NodeId,
    live_ctrl: ValueId,
) -> bool {
    let mut seen: DenseEntitySet<NodeId> = DenseEntitySet::new();
    let mut stack: Vec<NodeId> = edit.value_uses(live_ctrl).map(|(node, _)| node).collect();
    while let Some(node) = stack.pop() {
        // `root` is about to go, and its live successors are already seeded.
        if node == root || !seen.insert(node) {
            continue;
        }
        if matches!(
            edit.node_kind(node),
            NodeKind::Return | NodeKind::IndirectBranch | NodeKind::Unreachable
        ) {
            return true;
        }
        for &out in edit.node_outputs(node) {
            if !edit.value_kind(out).is_control() {
                continue;
            }
            let mut consumed = false;
            for (succ, _) in edit.value_uses(out) {
                consumed = true;
                stack.push(succ);
            }
            if !consumed {
                return true;
            }
        }
    }
    false
}
