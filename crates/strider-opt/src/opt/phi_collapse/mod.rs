//! Braun trivial-phi elimination for `Phi` and `MemPhi`.
//!
//! Inputs are `[phi_token, val_0, val_1, ...]`. A phi is trivial when, ignoring
//! the token and discarding self-references, exactly ONE distinct value input
//! remains; every use of the phi is then redirected to it. Zero distinct values
//! (fully self-referential) and two-or-more (a genuine merge) are left alone.
//!
//! The data-side companion to [`crate::RegionCollapse`]. Neither pass touches
//! the other's node kinds, so they compose in any order.

use strider_ir::IRViewer;
use strider_ir::node::{NodeId, NodeKind, ValueId};

use crate::error::Result;
use crate::peephole::{PeepholePass, PeepholeRewrite};

#[cfg(test)]
mod tests;

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
        // Both the seed walk and the consumer re-enqueue filter on
        // `matches_kind`, so `root` is always a `Phi`/`MemPhi` and the
        // single-output assumption below holds.
        let inputs = edit.node_inputs(root);
        // A well-formed phi has at least its token; without one there is
        // nothing to collapse.
        if inputs.is_empty() {
            return Ok(PeepholeRewrite::NoChange);
        }

        let phi_value = edit
            .node_outputs_exact::<1>(root)
            .expect("Phi / MemPhi has 1 output per node signature")[0];

        // An `Option` accumulator rather than a set: a `DenseEntitySet` sizes
        // its bitvector to the max `ValueId` index just to hold the one or two
        // distinct values a phi can have.
        let mut unique: Option<ValueId> = None;
        for value in edit.phi_data_inputs(root) {
            if value == phi_value {
                continue; // loop-carried self-reference (Braun): ignore
            }
            match unique {
                None => unique = Some(value),
                Some(u) if u == value => {}
                Some(_) => return Ok(PeepholeRewrite::NoChange), // genuine merge
            }
        }
        let Some(unique) = unique else {
            return Ok(PeepholeRewrite::NoChange);
        };

        // Collapses onto an existing value, hence `new_node: None` below.
        let changed = edit.replace_value(phi_value, unique)?;
        // The automatic cull would reach this phi eventually, but only at
        // end-of-iteration. Killing it inline drops it from the live set THIS
        // sweep so it stops counting as a live consumer of its Region's
        // phi-token, letting `RegionCollapse` detach that Region in the same
        // iteration. Unconditional: a trivial phi is a no-op whether or not any
        // consumer was actually redirected.
        edit.kill_node(root);
        Ok(if changed {
            PeepholeRewrite::Changed { new_node: None }
        } else {
            PeepholeRewrite::NoChange
        })
    }

    /// A collapse can make a consumer phi trivial too (the redirect removes one
    /// distinct operand downstream), so the cascade fires in the same sweep.
    fn propagate_to_consumers(&self) -> bool {
        true
    }
}
