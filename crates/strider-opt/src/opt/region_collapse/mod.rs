//! Collapses single-predecessor `Region` joins: a `Region` with exactly ONE
//! control input is a no-op, so its control output is replaced by that input.
//!
//! Phis layered over the Region are [`crate::PhiCollapse`]'s job; this pass
//! deliberately leaves them alone. The dead Region is killed only once both its
//! outputs have no live uses, since `Region` is side-effecting and the automatic
//! dead-cone cull never reaches it. Until then it stays attached; a fully
//! unreachable orphan is harmless (validator and pattern queries walk from
//! entry).

use strider_ir::IRViewer;
use strider_ir::node::{NodeId, NodeKind};

use crate::error::Result;
use crate::pipeline::{OptCtx, OptimizationResult, Optimizer};

#[cfg(test)]
mod tests;

#[derive(Clone, Copy)]
pub struct RegionCollapse;

impl Optimizer for RegionCollapse {
    fn apply(
        &self,
        edit: &mut crate::EditFunction<'_>,
        _opt: &mut OptCtx<'_>,
    ) -> Result<OptimizationResult> {
        // One walk of the cached live Regions, no worklist. `try_collapse` reads
        // each root's CURRENT inputs and liveness, so edits made earlier in this
        // loop are visible to later entries. A collapse that exposes a
        // downstream single-pred Region needs no re-enqueue: either that Region
        // comes later in this list, or the outer fixed-point loop catches it.
        let regions: Vec<NodeId> = edit
            .live_of_kind(|k| matches!(k, NodeKind::Region))
            .collect();
        let mut overall = OptimizationResult::NoChange;
        for root in regions {
            if self.try_collapse(edit, root)?.changed() {
                overall = OptimizationResult::Changed;
            }
        }
        Ok(overall)
    }
}

impl RegionCollapse {
    fn try_collapse(
        &self,
        edit: &mut crate::EditFunction<'_>,
        root: NodeId,
    ) -> Result<OptimizationResult> {
        // The caller filters on `NodeKind::Region`, so the output layout read
        // below is sound.
        let inputs = edit.node_inputs(root);
        // The entry Region (0 inputs) and genuine multi-way joins stay put.
        if inputs.len() != 1 {
            return Ok(OptimizationResult::NoChange);
        }
        let sole_ctrl_value = inputs[0];
        // Region outputs are [control, phi_token].
        let [ctrl_value, _phi_token] = edit.node_outputs_exact::<2>(root)?;
        // `replace_value` also absorbs the old producer's asm fingerprint into
        // the new one.
        let result =
            OptimizationResult::from_changed(edit.replace_value(ctrl_value, sole_ctrl_value)?);

        // Detach only once BOTH outputs (control AND phi_token) have no live
        // uses. Leaving it attached earlier would keep the Region as a second
        // consumer of `sole_ctrl_value`, breaking forward single-consumer walks
        // like `IfPat::true_branch` and keeping the node control-reachable.
        //
        // Liveness, not use count: an orphan `Phi`/`MemPhi` (a builder-emitted
        // VarPhi for a register nobody reads, or one already dead when
        // `PhiCollapse` swept) is unreachable arena residue that nothing sweeps,
        // and counting it would pin the Region forever. A LIVE phi-token
        // consumer does block the detach; `PhiCollapse` collapses it and a later
        // iteration finishes the job.
        let all_outputs_unused = edit.node_outputs(root).iter().all(|&out| {
            edit.graph_ref()
                .value_uses(out)
                .all(|(consumer, _)| !edit.is_live(consumer))
        });
        if all_outputs_unused && !edit.node_inputs(root).is_empty() {
            // `Region` is side-effecting, so nothing culls it automatically.
            edit.kill_node(root);
        }
        Ok(result)
    }
}
