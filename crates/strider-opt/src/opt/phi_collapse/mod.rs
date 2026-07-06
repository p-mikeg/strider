//! `PhiCollapse` — Braun trivial-phi elimination for `Phi` and `MemPhi`.
//!
//! A `Phi` / `MemPhi` has inputs `[phi_token, val_0, val_1, …]`.  It is
//! *trivial* when, after (a) skipping the `phi_token` at index 0 and (b)
//! discarding self-references (value inputs equal to the phi's own
//! output), the remaining distinct value outputs number exactly **one**
//! — call it `V`.  The phi is then a no-op and every use of its output is
//! redirected to `V` via [`crate::EditFunction::replace_value`].
//!
//! When zero distinct values remain (a fully self-referential phi, or one
//! with no real input) or two-or-more distinct values remain (a genuine
//! merge), the phi is left unchanged.
//!
//! This is the data-side companion to [`crate::RegionCollapse`]: the
//! latter collapses single-predecessor `Region` joins; this pass collapses
//! the trivial phis layered over any join.  Neither pass touches the
//! other's node kinds, so they compose without ordering constraints.

use strider_ir::IRViewer;
use strider_ir::node::{NodeId, NodeKind, ValueId};

use crate::error::Result;
use crate::peephole::{PeepholePass, PeepholeRewrite};

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
        edit: &mut crate::EditFunction<'_>,
        _opt_ctx: &mut crate::pipeline::OptCtx<'_>,
        root: NodeId,
    ) -> Result<PeepholeRewrite> {
        // `run_peephole` only hands us nodes matching `matches_kind`
        // (`Phi`/`MemPhi`) — both the seed walk and the consumer re-enqueue
        // filter on kind — so the single-value-output assumption below holds.
        let inputs = edit.node_inputs(root);
        // A well-formed phi has at least `[phi_token]`; without a token
        // there is nothing to collapse.
        if inputs.is_empty() {
            return Ok(PeepholeRewrite::NoChange);
        }

        // The phi's own output — used to discard the loop-carried
        // self-reference (Braun's trivial-phi rule).
        let phi_value = edit
            .node_outputs_exact::<1>(root)
            .expect("Phi / MemPhi has 1 output per node signature")[0];

        // Find the single distinct non-self value input, bailing the moment a
        // second distinct value appears (a genuine merge).  No allocation: a
        // `DenseEntitySet` would size a bitvector to the max `ValueId`
        // index — O(max_index) — just to hold the 1–2 distinct values of a
        // phi.  A linear scan with an `Option` accumulator short-circuits on
        // the second distinct value and yields `unique` in the same pass.
        let mut unique: Option<ValueId> = None;
        for value in edit.phi_data_inputs(root) {
            if value == phi_value {
                continue; // loop-carried self-reference (Braun): ignore
            }
            match unique {
                None => unique = Some(value),
                Some(u) if u == value => {} // same value again — fine
                Some(_) => return Ok(PeepholeRewrite::NoChange), // ≥2 distinct → genuine merge
            }
        }
        // Zero distinct values (fully self-referential / no real input):
        // leave it alone.
        let Some(unique) = unique else {
            return Ok(PeepholeRewrite::NoChange);
        };

        // Collapse to an EXISTING value (`unique`) — no fresh node — so report
        // `new_node: None`.  Consumer re-enqueue (driven by
        // `propagate_to_consumers`) handles the cascade.
        let changed = edit.replace_value(phi_value, unique)?;
        // The phi's sole output is now unused (consumers rewired to `unique`),
        // so kill it.  `Phi`/`MemPhi` are not side-effecting, so the automatic
        // cull WOULD reach the collapsed phi after the next `clean()` drain —
        // but only at end-of-iteration.  Killing it inline removes it from the
        // live set THIS sweep so it stops counting as a live consumer of its
        // owning Region's phi-token, letting `RegionCollapse` detach that
        // Region in the same iteration (deferring to auto-clean would push that
        // to a later iteration).  `kill_node` also auto-enqueues the now-dead
        // value-input cones for `clean` to cascade-cull.  This mirrors the
        // former `RedundantPhis` policy of unconditionally detaching the
        // collapsed phi's inputs (the trivial phi is a no-op regardless of
        // whether any consumer was actually redirected).
        edit.kill_node(root);
        Ok(if changed {
            PeepholeRewrite::Changed { new_node: None }
        } else {
            PeepholeRewrite::NoChange
        })
    }

    /// A collapse can make a *consumer* phi trivial (the redirected value
    /// removes one distinct operand from a downstream phi), so re-enqueue
    /// consumers to let the cascade fire in the same sweep.
    fn propagate_to_consumers(&self) -> bool {
        true
    }
}
