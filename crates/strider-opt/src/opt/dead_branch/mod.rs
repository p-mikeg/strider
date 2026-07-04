//! `DeadBranchElimination` — folds a control branch with a constant selector:
//! an `If` with a constant `I1` condition, or a `Switch` with a constant
//! dispatch address.
//!
//! For `If(ctrl_in, IntConst(b):I1)` with outputs `[ctrl_true, ctrl_false]`,
//! the **live** control output (`ctrl_true` when `b = true`, else
//! `ctrl_false`) is replaced with `ctrl_in` so the live successor receives
//! control directly, and the now-folded `If` is **killed unconditionally**
//! (`If` is side-effecting, so the automatic dead-cone cull never reaches it).
//! A `Switch(ctrl_in, IntConst(K))` is handled identically: the output whose
//! case address equals `K` is the live one, and the `Switch` is killed.
//!
//! Killing the `If` severs the only edge keeping it on the live walk, so
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

use strider_ir::IRViewer;
use strider_ir::node::{NodeId, NodeKind};

use crate::error::Result;
use crate::peephole::{PeepholePass, PeepholeRewrite};

#[cfg(test)]
mod tests;

/// Eliminates control branches with a compile-time-constant selector: an `If`
/// whose condition is a constant `I1` (folds to the live arm), and a `Switch`
/// whose dispatch address is a constant (collapses to the arm whose case
/// address matches).
///
/// Works together with [`crate::CfgDetach`] (which removes the dead
/// `Region` predecessor slot) and [`crate::PhiCollapse`] /
/// [`crate::RegionCollapse`] (which collapse the now-single-pred join).
#[derive(Clone, Copy)]
pub struct DeadBranchElimination;

impl PeepholePass for DeadBranchElimination {
    fn matches_kind(&self, kind: &NodeKind) -> bool {
        matches!(kind, NodeKind::If | NodeKind::Switch)
    }

    fn try_rewrite(
        &self,
        ctx: &mut crate::EditFunction<'_>,
        _opt_ctx: &mut crate::pipeline::OptCtx<'_>,
        root: NodeId,
    ) -> Result<PeepholeRewrite> {
        match ctx.node_kind(root) {
            NodeKind::If => {
                // If inputs: [ctrl_in, condition] — exactly 2 (validated arity).
                // This pass neither re-enqueues consumers nor reports a `new_node`
                // (`propagate_to_consumers` is `false`), so a detached If is never
                // handed back to `try_rewrite`; `root` always carries its original
                // two inputs.
                let [ctrl_value, cond_value] = ctx
                    .graph_ref()
                    .node_inputs_exact::<2>(root)
                    .expect("If has 2 inputs per node signature");

                let Some(cond_val) = ctx.function().bool_const_val(cond_value) else {
                    return Ok(PeepholeRewrite::NoChange);
                };

                // If outputs: [ctrl_true (index 0), ctrl_false (index 1)].
                let [ctrl_true, ctrl_false] = ctx
                    .node_outputs_exact::<2>(root)
                    .expect("If has 2 outputs per node signature");
                let live_ctrl = if cond_val { ctrl_true } else { ctrl_false };

                // The **condition** is part of the proof for killing this branch: we
                // take the live successor unconditionally only because `cond_value`
                // folded to a compile-time constant.  Its asm-fingerprint (the
                // cmp/flag/const-fold cone) would otherwise be lost when `kill_node`
                // cascade-culls the now-dead condition cone, so absorb it into the
                // surviving control source (the producer of `ctrl_value`, which the
                // `replace_value` below makes the live successor's control input).
                // Over-tainting is intentional — the fingerprint is a superset
                // proof-of-correctness aid, not a minimal value-determining set.
                ctx.absorb_fingerprint(ctrl_value, cond_value);

                // Redirect the live successor past the If, then explicitly kill the
                // folded If — CfgDetach + validation (run only at pipeline
                // convergence) own the escape case.  `If` is side-effecting, so the
                // automatic dead-cone cull never reaches it; the explicit
                // `kill_node` removes it from the live graph AND auto-enqueues its
                // now-dead pure operands (the folded `IntConst` condition cone) for
                // `clean` to cascade-cull.  This is a pure control redirect to an
                // EXISTING edge — no fresh node — so report `new_node: None`.
                ctx.replace_value(live_ctrl, ctrl_value)?;
                ctx.kill_node(root);
                Ok(PeepholeRewrite::Changed { new_node: None })
            }
            NodeKind::Switch => {
                // Switch inputs: [ctrl, address] (exactly 2 per node signature).
                let [ctrl_value, addr_value] = ctx
                    .graph_ref()
                    .node_inputs_exact::<2>(root)
                    .expect("Switch has 2 inputs per node signature");

                let Some(k) = ctx.function().int_const_u128(addr_value) else {
                    return Ok(PeepholeRewrite::NoChange);
                };

                // Find the output whose case address == K (output i ↔
                // cases[i]).  Computing the index inside this expression lets
                // the immutable borrow of `ctx.function()` end here — before
                // the mutable calls below — without cloning the address slice.
                let Some(i) = ctx
                    .function()
                    .side_tables()
                    .switch_targets(root)
                    .iter()
                    .position(|&t| u128::from(t) == k)
                else {
                    return Ok(PeepholeRewrite::NoChange); // exhaustive table => shouldn't happen
                };
                let live_ctrl = ctx.node_outputs(root)[i];

                // Same proof-completeness rationale as the `If` arm above:
                // the constant dispatch address is part of the proof for
                // killing this Switch, so absorb its fingerprint into the
                // surviving control source before the address cone is
                // cascade-culled by `kill_node`.
                ctx.absorb_fingerprint(ctrl_value, addr_value);
                ctx.replace_value(live_ctrl, ctrl_value)?;
                ctx.kill_node(root);
                Ok(PeepholeRewrite::Changed { new_node: None })
            }
            _ => Ok(PeepholeRewrite::NoChange),
        }
    }

    /// Folding an `If` redirects control and detaches the node; it doesn't
    /// fold a value into a constant, so re-enqueueing consumers would only
    /// re-walk joins whose shape `CfgDetach` cleans up separately.
    fn propagate_to_consumers(&self) -> bool {
        false
    }
}
