//! Collapses single-predecessor `Region` joins: a `Region` with exactly ONE
//! control input is a no-op, so its control output is replaced by that input
//! and every phi over it by that phi's single value input.
//!
//! The Region is then killed: it is side-effecting, so the automatic
//! dead-cone cull never reaches it.
//!
//! A `Region` unreachable from entry is skipped: its sole predecessor can be
//! its own `If`, and a phi over it then collapses onto a value built from
//! itself, which `validate` rejects as a non-phi data cycle.

use strider_ir::IRViewer;
use strider_ir::node::{NodeId, NodeKind, ValueId};

use crate::Result;
use crate::pipeline::{OptCtx, Optimizer};

#[cfg(test)]
mod tests;

pub struct RegionCollapse;

impl Optimizer for RegionCollapse {
    fn apply(&self, edit: &mut crate::EditFunction<'_>, _opt: &OptCtx<'_>) -> Result<()> {
        // One walk of the cached live Regions, no worklist. `try_collapse` reads
        // each root's CURRENT inputs and liveness, so edits made earlier in this
        // loop are visible to later entries.
        let reachable = strider_ir::walk::cfg_reachable(edit.function().graph(), edit.entry());
        let regions: Vec<NodeId> = edit
            .live_of_kind(|k| matches!(k, NodeKind::Region))
            .filter(|&region| reachable.contains(region))
            .collect();
        for root in regions {
            try_collapse(edit, root);
        }
        Ok(())
    }
}

fn try_collapse(edit: &mut crate::EditFunction<'_>, root: NodeId) {
    let inputs = edit.node_inputs(root);
    // The entry Region is NOT input-less: `Entry` is its one predecessor,
    // so an empty entry collapses like any other.
    if inputs.len() != 1 {
        return;
    }
    let sole_ctrl_value = inputs[0];
    // Region outputs are [control, phi_token].
    let [ctrl_value, phi_token] = edit
        .node_outputs_exact::<2>(root)
        .expect("a Region has a control output and a phi token");

    // Order matters: while a phi over the token is live the Region cannot
    // detach, and an attached Region left behind by the rewire below is a
    // SECOND consumer of `sole_ctrl_value`, which `validate` rejects.
    if !collapse_region_phis(edit, phi_token) {
        return;
    }

    // A control output with no consumer makes the rewire itself a no-op.
    // `replace_value` also absorbs the old producer's asm fingerprint into
    // the new one.  With every live phi over the token gone, nothing live
    // consumes the Region.
    edit.replace_value(ctrl_value, sole_ctrl_value);
    edit.kill_node(root);
}

/// Replaces every live phi over `phi_token` with its single value input and
/// kills it: with one control predecessor a phi merges nothing.
///
/// `false`, having edited nothing, if any phi does not have exactly one value
/// input, or draws its value from another phi over this token: the collapse pairs are
/// snapshotted, so such a value is retargeted by an earlier replacement and
/// then killed, leaving it dangling.
fn collapse_region_phis(edit: &mut crate::EditFunction<'_>, phi_token: ValueId) -> bool {
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
        let [phi_value] = edit
            .node_outputs_exact::<1>(phi)
            .expect("a phi has one output");
        let data: Vec<ValueId> = edit.phi_data_inputs(phi).collect();
        let [sole] = data[..] else {
            return false;
        };
        if phi_set.contains(edit.producer(sole)) {
            return false;
        }
        collapses.push((phi_value, sole));
    }
    for (phi_value, sole) in collapses {
        edit.replace_value(phi_value, sole);
    }
    for phi in phis {
        edit.kill_node(phi);
    }
    true
}
