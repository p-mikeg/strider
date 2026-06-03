//! `DeadBranchElimination` — folds an `If` with a constant `I1` condition.
//!
//! For `If(ctrl_in, IntConst(b):I1)` with outputs `[ctrl_true, ctrl_false]`,
//! the **live** control output (`ctrl_true` when `b = true`, else
//! `ctrl_false`) is replaced with `ctrl_in` so the live successor receives
//! control directly, and the now-folded `If`'s inputs are detached
//! **unconditionally**.
//!
//! Detaching the `If` severs the only edge keeping it on the live walk, so
//! the outer fixed-point loop stops re-visiting it.  When a dead-branch
//! subgraph still escapes to live data (e.g. a dead `Call`'s `mem_value`
//! flowing into a live `MemPhi`), the unconditional detach can momentarily
//! leave the dead subgraph reachable through backward-data — but
//! [`crate::CfgDetach`] then severs the dead `Region`-predecessor edge,
//! and [`crate::PhiCollapse`] finishes the teardown.  Any node left
//! fully unreachable stays in the arena untouched (the validator and
//! pattern queries only walk from entry).  Validation runs only after the whole destructive
//! pipeline converges, so the transient escaping shape is never observed by
//! the validator.  (This is the soundness `CfgDetach`'s
//! `cfg_detach_collapses_var_and_mem_phi_then_validates` test already
//! proved.)
//!
//! This pass no longer strips `Region` predecessor slots — that surgery is
//! [`crate::CfgDetach`]'s sole responsibility now.

use strider_ir::node::{NodeId, NodeKind};

use crate::error::Result;
use crate::peephole::{PeepholePass, PeepholeRewrite};

#[cfg(test)]
mod tests;

/// Eliminates branches whose condition is a compile-time boolean constant.
///
/// Works together with [`crate::CfgDetach`] (which removes the dead
/// `Region` predecessor slot) and [`crate::PhiCollapse`] /
/// [`crate::RegionCollapse`] (which collapse the now-single-pred join).
#[derive(Clone, Copy)]
pub struct DeadBranchElimination;

impl PeepholePass for DeadBranchElimination {
    fn matches_kind(&self, kind: &NodeKind) -> bool {
        matches!(kind, NodeKind::If)
    }

    fn try_rewrite(
        &self,
        ctx: &mut crate::RewriteCtx<'_>,
        root: NodeId,
    ) -> Result<PeepholeRewrite> {
        // If inputs: [ctrl_in, condition] — exactly 2 (validated arity).
        // This pass neither re-enqueues consumers nor reports a `new_node`
        // (`propagate_to_consumers` is `false`), so a detached If is never
        // handed back to `try_rewrite`; `root` always carries its original
        // two inputs.
        let [ctrl_value, cond_value] = ctx
            .graph_ref()
            .node_inputs_exact::<2>(root)
            .expect("If has 2 inputs per node signature");

        let Some(cond_val) = ctx.graph_ref().bool_const_val(cond_value) else {
            return Ok(PeepholeRewrite::NoChange);
        };

        // If outputs: [ctrl_true (index 0), ctrl_false (index 1)].
        let [ctrl_true, ctrl_false] = ctx
            .node_outputs_exact::<2>(root)
            .expect("If has 2 outputs per node signature");
        let live_ctrl = if cond_val { ctrl_true } else { ctrl_false };

        // Redirect the live successor past the If, then detach the folded
        // If unconditionally — CfgDetach + validation (run only at pipeline
        // convergence) own the escape case.  This is a pure control
        // redirect to an EXISTING edge — no fresh node — so report
        // `new_node: None`.
        ctx.replace_value(live_ctrl, ctrl_value)?;
        ctx.detach_node_inputs(root);
        Ok(PeepholeRewrite::Changed { new_node: None })
    }

    /// Folding an `If` redirects control and detaches the node; it doesn't
    /// fold a value into a constant, so re-enqueueing consumers would only
    /// re-walk joins whose shape `CfgDetach` cleans up separately.
    fn propagate_to_consumers(&self) -> bool {
        false
    }
}
