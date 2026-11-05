//! Collapses a `Region` with exactly one control input: its control output is
//! replaced by that input, every phi over it by the phi's single value input,
//! and the Region is killed (culling never reaches it).
//!
//! An unreachable `Region` is skipped: its sole predecessor can be its own
//! `If`, and collapsing a phi there builds a data cycle.

use strider_ir::IRViewer;
use strider_ir::node::{NodeId, NodeKind, ValueId};

use crate::Result;
use crate::pipeline::{OptCtx, Optimizer};

#[cfg(test)]
mod tests;

pub struct RegionCollapse;

impl Optimizer for RegionCollapse {
    fn apply(&self, edit: &mut crate::EditFunction<'_>, _opt: &OptCtx<'_>) -> Result<()> {
        // `try_collapse` reads each root's current inputs, so earlier edits in
        // this loop are visible.
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
    // The entry Region has `Entry` as its one predecessor.
    if inputs.len() != 1 {
        return;
    }
    let sole_ctrl_value = inputs[0];
    let [ctrl_value, phi_token] = edit
        .node_outputs_exact::<2>(root)
        .expect("a Region has a control output and a phi token");

    // Phis first: a Region still attached after the rewire below would be a
    // second consumer of `sole_ctrl_value`.
    if !collapse_region_phis(edit, phi_token) {
        return;
    }

    edit.replace_value(ctrl_value, sole_ctrl_value);
    edit.kill_node(root);
}

/// Replaces every live phi over `phi_token` with its single value input and
/// kills it. `false`, having edited nothing, if any phi does not have exactly
/// one value input or draws it from another phi over this token.
fn collapse_region_phis(edit: &mut crate::EditFunction<'_>, phi_token: ValueId) -> bool {
    let phis: Vec<NodeId> = edit
        .graph_ref()
        .value_uses(phi_token)
        .map(|(consumer, _)| consumer)
        .filter(|&consumer| edit.is_live(consumer))
        .collect();
    let mut collapses: Vec<(ValueId, ValueId)> = Vec::with_capacity(phis.len());
    for &phi in &phis {
        let [phi_value] = edit
            .node_outputs_exact::<1>(phi)
            .expect("a phi has one output");
        let data: Vec<ValueId> = edit.phi_data_inputs(phi).collect();
        let [sole] = data[..] else {
            return false;
        };
        if phis.contains(&edit.producer(sole)) {
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
