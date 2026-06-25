//! `RegionCollapse` — collapses single-predecessor `Region` joins.
//!
//! A `Region` with exactly ONE control input is a no-op join: control
//! flows straight through it.  This pass replaces the Region's control
//! output (output index 0) with its lone control input, so every control
//! consumer connects directly to the predecessor's control producer.
//!
//! Phis layered over the Region are handled independently by
//! [`crate::PhiCollapse`]; this pass deliberately does **not** touch
//! them.  Once both of the Region's outputs have no remaining *live* uses
//! (checked via [`crate::EditFunction::is_live`]) this pass kills the
//! now-dead Region (it is side-effecting, so the automatic dead-cone cull
//! never reaches it); until then the now-dead Region is left attached (a
//! fully-unreachable orphan is harmless — the validator and pattern
//! queries only walk from entry).

use strider_ir::{
    IRViewer,
    node::{NodeId, NodeKind},
};

use crate::{
    error::Result,
    pipeline::{OptCtx, OptimizationResult, Optimizer},
};

#[cfg(test)]
mod tests;

/// Collapses a single-control-input `Region` by rewiring its control
/// consumers to its lone predecessor.
#[derive(Clone, Copy)]
pub struct RegionCollapse;

impl Optimizer for RegionCollapse {
    fn apply(
        &self,
        ctx: &mut crate::EditFunction<'_>,
        _opt: &mut OptCtx<'_>,
    ) -> Result<OptimizationResult> {
        // Walk the cached live `Region`s once — no worklist.  The liveness
        // check in `try_collapse` uses `EditFunction::is_live`, which reflects
        // the maintained live-set and is updated by each edit, so detaching a
        // Region earlier in this list is immediately visible for later entries.
        // Collapsing a Region that exposes a downstream single-pred Region is
        // handled without an explicit re-enqueue: if the newly-exposed Region
        // appears later in this list it folds in the same pass, and otherwise
        // the outer fixed-point loop (RegionCollapse is a main-loop pass)
        // re-runs until nothing collapses.  `try_collapse` reads each root's
        // CURRENT inputs, so an edit earlier in the pass is observed here.
        let regions: Vec<NodeId> = ctx
            .live_of_kind(|k| matches!(k, NodeKind::Region))
            .collect();
        let mut overall = OptimizationResult::NoChange;
        for root in regions {
            if self.try_collapse(ctx, root)?.changed() {
                overall = OptimizationResult::Changed;
            }
        }
        Ok(overall)
    }
}

impl RegionCollapse {
    fn try_collapse(
        &self,
        ctx: &mut crate::EditFunction<'_>,
        root: NodeId,
    ) -> Result<OptimizationResult> {
        // Both the seed walk and the consumer re-enqueue filter on
        // `NodeKind::Region`, so `root` is always a `Region` here and the
        // Region output layout read below is sound.
        let inputs = ctx.node_inputs(root);
        // Only a single-predecessor join is a no-op; the entry Region
        // (0 inputs) and genuine multi-way joins are left untouched.
        if inputs.len() != 1 {
            return Ok(OptimizationResult::NoChange);
        }
        let sole_ctrl_value = inputs[0];
        // Region outputs are [control, phi_token]; the control output is
        // index 0.
        let [ctrl_value, _phi_token] = ctx.node_outputs_exact::<2>(root)?;
        let result =
            OptimizationResult::NoChange.after_replace(ctx, ctrl_value, sole_ctrl_value)?;

        // After rewiring the control consumers, detach the now-dead Region's
        // own input edge — but ONLY once BOTH of its outputs (control AND
        // phi_token) have no remaining *live* uses.  Otherwise the Region
        // lingers as a second consumer of `sole_ctrl_value`, which breaks a
        // forward single-consumer walk (e.g. `IfPat::true_branch`) and keeps
        // the node control-reachable.
        //
        // A phi-token consumer only counts if it is live (in the maintained
        // live-set via `EditFunction::is_live`).  An orphan `Phi`/`MemPhi`
        // (e.g. a builder-emitted single-pred VarPhi for a register that's
        // never read, or a phi `PhiCollapse` rewired past but couldn't visit
        // because it was already dead — possibly chained behind other dead phis)
        // is harmless residue in the arena, never swept (the validator and
        // pattern queries only walk from entry).  Counting such orphans as live
        // uses would pin the Region forever.
        //
        // When a *live* `Phi`/`MemPhi` still consumes the phi-token, leave the
        // Region attached this iteration — `PhiCollapse` will collapse that phi,
        // after which a later iteration finds both outputs free and finishes the
        // detach.
        let all_outputs_unused = ctx.node_outputs(root).iter().all(|&out| {
            ctx.graph_ref()
                .value_uses(out)
                .all(|(consumer, _)| !ctx.is_live(consumer))
        });
        if all_outputs_unused && !ctx.node_inputs(root).is_empty() {
            // `Region` is side-effecting, so the automatic dead-cone cull
            // never reaches it — remove it explicitly.  `kill_node` detaches
            // its lone control-input edge (matching the former
            // `detach_node_inputs`), evicts it from the live set, and
            // auto-enqueues that now-orphaned predecessor cone for `clean`.
            ctx.kill_node(root);
        }
        Ok(result)
    }
}
