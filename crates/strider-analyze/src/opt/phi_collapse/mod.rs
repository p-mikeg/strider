//! `PhiCollapse` — Braun trivial-phi elimination for `Phi` and `MemPhi`.
//!
//! A `Phi` / `MemPhi` has inputs `[phi_token, val_0, val_1, …]`.  It is
//! *trivial* when, after (a) skipping the `phi_token` at index 0 and (b)
//! discarding self-references (value inputs equal to the phi's own
//! output), the remaining distinct value outputs number exactly **one**
//! — call it `V`.  The phi is then a no-op and every use of its output is
//! redirected to `V` via [`strider_pattern::RewriteCtx::replace_value`].
//!
//! When zero distinct values remain (a fully self-referential phi, or one
//! with no real input) or two-or-more distinct values remain (a genuine
//! merge), the phi is left unchanged.
//!
//! This is the data-side companion to [`crate::opt::RegionCollapse`]: the
//! latter collapses single-predecessor `Region` joins; this pass collapses
//! the trivial phis layered over any join.  Neither pass touches the
//! other's node kinds, so they compose without ordering constraints.

use entity_utils::DenseEntitySet;
use strider_ir::node::{NodeId, NodeKind, NodeOutputId};

use crate::opt::error::Result;
use crate::opt::peephole::{PeepholePass, PeepholeRewrite};
use crate::opt::OptRewrite;

#[cfg(test)]
mod tests;

/// Braun trivial-phi elimination on `Phi` / `MemPhi`.
///
/// Collapses a phi whose only non-self-referential value inputs all
/// resolve to a single distinct value output, rewiring its consumers
/// directly to that value.
#[derive(Clone, Copy)]
pub struct PhiCollapse;

impl PeepholePass for PhiCollapse {
    fn matches_kind(&self, kind: &NodeKind) -> bool {
        matches!(kind, NodeKind::Phi | NodeKind::MemPhi)
    }

    fn try_rewrite(
        &self,
        ctx: &mut strider_pattern::RewriteCtx<'_>,
        root: NodeId,
    ) -> Result<PeepholeRewrite> {
        // The peephole driver re-enqueues *consumers* (any kind) after a
        // collapse, so `try_rewrite` can be handed a non-phi node — guard
        // on kind before touching the single-value-output assumption.
        if !matches!(ctx.node_kind(root), NodeKind::Phi | NodeKind::MemPhi) {
            return Ok(PeepholeRewrite::NoChange);
        }

        let inputs = ctx.node_inputs(root);
        // A well-formed phi has at least `[phi_token]`; without a token
        // there is nothing to collapse.
        if inputs.is_empty() {
            return Ok(PeepholeRewrite::NoChange);
        }

        // The phi's own output — used to discard the loop-carried
        // self-reference (Braun's trivial-phi rule).
        let phi_out = ctx.node_outputs_exact::<1>(root)?[0];

        // Gather the distinct value outputs, skipping `phi_token`
        // (index 0) and discarding self-references.
        let mut distinct: DenseEntitySet<NodeOutputId> = DenseEntitySet::new();
        for value in inputs.into_iter().skip(1) {
            if value != phi_out {
                distinct.insert(value);
            }
        }

        let mut iter = distinct.iter();
        match (iter.next(), iter.next()) {
            // Exactly one distinct non-self value: the phi is trivial.
            (Some(unique), None) => {
                // Collapse to an EXISTING value (`unique`) — no fresh node —
                // so report `new_node: None`.  Consumer re-enqueue (driven
                // by `propagate_to_consumers`) handles the cascade.
                let changed = ctx.replace_value(phi_out, unique)?;
                // The phi's sole output is now unused (consumers rewired to
                // `unique`), so detach its input edges.  Leaving them attached
                // keeps the collapsed phi a live consumer of its owning
                // Region's phi-token, which would block `RegionCollapse` from
                // detaching that Region (its phi-token would still show a
                // use).  This mirrors the former `RedundantPhis` policy of
                // detaching the collapsed phi's inputs.
                ctx.detach_node_inputs(root);
                Ok(if changed {
                    PeepholeRewrite::Changed { new_node: None }
                } else {
                    PeepholeRewrite::NoChange
                })
            }
            // Zero (fully self-referential / no real input) or ≥2 distinct
            // values (a genuine merge): leave it alone.
            _ => Ok(PeepholeRewrite::NoChange),
        }
    }

    /// A collapse can make a *consumer* phi trivial (the redirected value
    /// removes one distinct operand from a downstream phi), so re-enqueue
    /// consumers to let the cascade fire in the same sweep.
    fn propagate_to_consumers(&self) -> bool {
        true
    }
}
