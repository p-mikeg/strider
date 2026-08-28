//! Collapses single-predecessor `Region` joins: a `Region` with exactly ONE
//! control input is a no-op, so its control output is replaced by that input
//! and every phi over it by that phi's single value input.
//!
//! The Region is killed once nothing live consumes its outputs: it is
//! side-effecting, so the automatic dead-cone cull never reaches it.

use strider_ir::IRViewer;
use strider_ir::node::{NodeId, NodeKind, ValueId};

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
        // loop are visible to later entries.
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
        let inputs = edit.node_inputs(root);
        // The entry Region is NOT input-less: `Entry` is its one predecessor,
        // so an empty entry collapses like any other.
        if inputs.len() != 1 {
            return Ok(OptimizationResult::NoChange);
        }
        let sole_ctrl_value = inputs[0];
        // Region outputs are [control, phi_token].
        let [ctrl_value, phi_token] = edit.node_outputs_exact::<2>(root)?;

        // Order matters: while a phi over the token is live the Region cannot
        // detach, and an attached Region left behind by the rewire below is a
        // SECOND consumer of `sole_ctrl_value`, which `validate` rejects.
        let Some(phis_collapsed) = collapse_region_phis(edit, phi_token)? else {
            return Ok(OptimizationResult::NoChange);
        };

        // A control output with no consumer makes the rewire itself a no-op.
        // `replace_value` also absorbs the old producer's asm fingerprint into
        // the new one.
        let rewired = edit.replace_value(ctrl_value, sole_ctrl_value)?;
        let mut result = OptimizationResult::from_changed(rewired || phis_collapsed);

        // Liveness, not use count: an orphan `Phi`/`MemPhi` is unreachable arena
        // residue that nothing sweeps, and counting it would pin the Region
        // forever.
        let all_outputs_unused = edit.node_outputs(root).iter().all(|&out| {
            edit.graph_ref()
                .value_uses(out)
                .all(|(consumer, _)| !edit.is_live(consumer))
        });
        if all_outputs_unused {
            edit.kill_node(root);
            result = OptimizationResult::Changed;
        }
        Ok(result)
    }
}

/// Replaces every live phi over `phi_token` with its single value input and
/// kills it: with one control predecessor a phi merges nothing.
///
/// `None`, having edited nothing, if any phi has more than one value input or
/// draws its value from another phi over this token: the collapse pairs are
/// snapshotted, so such a value is retargeted by an earlier replacement and
/// then killed, leaving it dangling.  `Some(true)` when at least one phi
/// collapsed.
fn collapse_region_phis(
    edit: &mut crate::EditFunction<'_>,
    phi_token: ValueId,
) -> Result<Option<bool>> {
    let phis: Vec<NodeId> = edit
        .graph_ref()
        .value_uses(phi_token)
        .map(|(consumer, _)| consumer)
        .filter(|&consumer| edit.is_live(consumer))
        .collect();
    let mut phi_set: entity_utils::DenseEntitySet<NodeId> = entity_utils::DenseEntitySet::new();
    for &phi in &phis {
        phi_set.insert(phi);
    }
    let mut collapses: Vec<(ValueId, ValueId)> = Vec::with_capacity(phis.len());
    for &phi in &phis {
        let [phi_value] = edit.node_outputs_exact::<1>(phi)?;
        let data: Vec<ValueId> = edit.phi_data_inputs(phi).collect();
        let [sole] = data[..] else {
            return Ok(None);
        };
        if phi_set.contains(edit.producer(sole)) {
            return Ok(None);
        }
        collapses.push((phi_value, sole));
    }
    for (phi_value, sole) in collapses {
        edit.replace_value(phi_value, sole)?;
    }
    let collapsed = !phis.is_empty();
    for phi in phis {
        edit.kill_node(phi);
    }
    Ok(Some(collapsed))
}
