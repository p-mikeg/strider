//! `RegionCollapse` — collapses single-predecessor `Region` joins.
//!
//! A `Region` with exactly ONE control input is a no-op join: control
//! flows straight through it.  This pass replaces the Region's control
//! output (output index 0) with its lone control input, so every control
//! consumer connects directly to the predecessor's control producer.
//!
//! Phis layered over the Region are handled independently by
//! [`crate::opt::PhiCollapse`]; this pass deliberately does **not** touch
//! them.  The Region node itself is left attached for the orphan-cleanup
//! sweep ([`crate::opt::DetachUnreachable`]) rather than detached here —
//! keeping each peephole's surgery minimal.

use strider_ir::node::{NodeId, NodeKind};

use crate::opt::error::Result;
use crate::opt::peephole::PeepholePass;
use crate::opt::pipeline::OptimizationResult;

#[cfg(test)]
mod tests;

/// Collapses a single-control-input `Region` by rewiring its control
/// consumers to its lone predecessor.
#[derive(Clone, Copy)]
pub struct RegionCollapse;

impl PeepholePass for RegionCollapse {
    fn matches_kind(&self, kind: &NodeKind) -> bool {
        matches!(kind, NodeKind::Region)
    }

    fn try_rewrite(
        &self,
        ctx: &mut strider_pattern::RewriteCtx<'_>,
        root: NodeId,
    ) -> Result<OptimizationResult> {
        // The peephole driver re-enqueues *consumers* (any kind) after a
        // collapse, so `try_rewrite` can be handed a non-Region node —
        // guard on kind before reading the Region output layout.
        if !matches!(ctx.node_kind(root), NodeKind::Region) {
            return Ok(OptimizationResult::NoChange);
        }

        let inputs = ctx.node_inputs(root);
        // Only a single-predecessor join is a no-op; the entry Region
        // (0 inputs) and genuine multi-way joins are left untouched.
        if inputs.len() != 1 {
            return Ok(OptimizationResult::NoChange);
        }
        let sole_ctrl_in = inputs[0];
        // Region outputs are [control, phi_token]; the control output is
        // index 0.
        let ctrl_out = ctx.node_outputs_exact::<2>(root)?[0];
        OptimizationResult::NoChange.after_replace(ctx, ctrl_out, sole_ctrl_in)
    }

    /// Collapsing a single-pred Region can expose a downstream Region as
    /// single-pred too, so let the cascade fire in the same sweep.
    fn propagate_to_consumers(&self) -> bool {
        true
    }
}
