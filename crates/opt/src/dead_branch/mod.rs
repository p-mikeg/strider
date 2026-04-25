use std::collections::VecDeque;

use rustc_hash::FxHashSet;

use ir::BuiltFunctionGraph;
use ir::node::{NodeId, NodeKind};

use crate::error::Result;
use crate::pipeline::{OptimizationResult, Optimizer};

#[cfg(test)]
mod tests;

// ── Local worklist (hoisted to crate::worklist in Task 2.I) ───────────────────

#[derive(Default)]
struct WorkSet {
    queued: FxHashSet<NodeId>,
    queue: VecDeque<NodeId>,
}

impl WorkSet {
    fn seeded(it: impl IntoIterator<Item = NodeId>) -> Self {
        let mut q = Self::default();
        for n in it {
            q.push(n);
        }
        q
    }
    fn push(&mut self, n: NodeId) {
        if self.queued.insert(n) {
            self.queue.push_back(n);
        }
    }
    fn pop(&mut self) -> Option<NodeId> {
        let n = self.queue.pop_front()?;
        self.queued.remove(&n);
        Some(n)
    }
}

// ── Dead-branch elimination ───────────────────────────────────────────────────

/// Eliminates `If` nodes whose condition is a `BoolConst`.
///
/// For `If(ctrl_in, BoolConst(b))` with outputs `[ctrl_true, ctrl_false]`:
///
/// * The **live** control output (`ctrl_true` when `b=true`, `ctrl_false` when
///   `b=false`) is replaced with `ctrl_in` so the successor region receives
///   control directly without going through the `If`.
/// * The **dead** control output is removed from the successor `ControlState`'s
///   input list, and the corresponding position is also removed from every
///   `ControlPhi` node of that region.
///
/// After this pass, dead `ControlState` nodes end up with zero control inputs
/// and `ControlPhi` nodes with a single value input; `RedundantPhis` then
/// cleans those up.
fn try_eliminate_dead_branch(
    fg: &mut BuiltFunctionGraph,
    node_id: NodeId,
) -> Result<OptimizationResult> {
    // Only handle If nodes.
    if !matches!(*fg.graph.node_kind(node_id), NodeKind::If) {
        return Ok(OptimizationResult::NoChange);
    }

    // If inputs: [ctrl_in, condition].
    let inputs = fg.graph.node_inputs(node_id);
    if inputs.len() < 2 {
        return Ok(OptimizationResult::NoChange);
    }
    let ctrl_in = inputs[0];
    let cond_out = inputs[1];

    let Some(cond_val) = fg.bool_const_val(cond_out) else {
        return Ok(OptimizationResult::NoChange);
    };

    // If outputs: [ctrl_true (index 0), ctrl_false (index 1)].
    let [ctrl_true, ctrl_false] = fg.graph.node_outputs_exact::<2>(node_id)?;

    let (live_ctrl, dead_ctrl) = if cond_val {
        (ctrl_true, ctrl_false)
    } else {
        (ctrl_false, ctrl_true)
    };

    // ── Step 1: collect dead-ctrl uses before any mutation ────────────────────
    // Each use is (ControlState node, input_index_in_that_node).
    let dead_uses: Vec<(NodeId, u32)> = fg.graph.output_uses(dead_ctrl).collect();

    // ── Step 2: replace live ctrl with ctrl_in (bypass the If) ───────────────
    fg.replace_all_uses(live_ctrl, ctrl_in)?;

    // ── Step 3: remove dead ctrl inputs from successor ControlState(s) ───────
    for (cs_node, dead_idx) in dead_uses {
        if !matches!(*fg.graph.node_kind(cs_node), NodeKind::ControlState) {
            continue; // Unexpected consumer kind; skip safely.
        }

        // ControlState outputs: [ctrl_out, phi_out].
        let cs_outputs = fg.graph.node_outputs(cs_node);
        if cs_outputs.len() < 2 {
            continue;
        }
        let cs_phi_out = cs_outputs[1];

        // Collect ControlPhi nodes that consume the phi token before we mutate.
        let phi_nodes: Vec<NodeId> = fg
            .graph
            .output_uses(cs_phi_out)
            .map(|(phi, _)| phi)
            .collect();

        // Remove the dead variable-value input from each ControlPhi.
        // ControlPhi inputs: [phi_token, val_from_pred0, val_from_pred1, …]
        // So the variable value for predecessor at ControlState index `dead_idx`
        // lives at ControlPhi index `dead_idx + 1`.
        let phi_input_idx = dead_idx + 1;
        for phi_node in phi_nodes {
            let phi_len = fg.graph.node_inputs(phi_node).len() as u32;
            if phi_input_idx < phi_len {
                fg.graph.remove_node_input(phi_node, phi_input_idx)?;
            }
        }

        // Remove the dead ctrl input from the ControlState itself.
        let cs_len = fg.graph.node_inputs(cs_node).len() as u32;
        if dead_idx < cs_len {
            fg.graph.remove_node_input(cs_node, dead_idx)?;
        }
    }

    // ── Step 4: detach the If's own inputs so the pre-order walker no longer
    // reaches it via `ctrl_in`. Without this the fixed-point loop would spin
    // forever: the If's outputs have no users but its inputs still tie it to
    // the reachable subgraph, so the walker re-visits it on every iteration.
    fg.graph.detach_node_inputs(node_id);

    Ok(OptimizationResult::Changed)
}

// ── Public optimizer ──────────────────────────────────────────────────────────

/// Eliminates branches whose condition is a compile-time boolean constant.
///
/// Works together with [`crate::RedundantPhis`]: after dead-branch elimination
/// the previously-live successor region may have a single-input `ControlState`
/// and `ControlPhi` nodes, which `RedundantPhis` can then collapse.
pub struct DeadBranchElimination;

impl Optimizer for DeadBranchElimination {
    fn optimize(&self, function: &mut BuiltFunctionGraph) -> crate::Result<OptimizationResult> {
        // DBE only fires on `If` nodes whose outputs are control edges; the
        // node it eliminates is never re-checked by this pass (only one If per
        // node id). A worklist with consumer re-enqueue gives no payoff here,
        // so we just drain the seeded preorder once.
        let mut work = WorkSet::seeded(function.preorder());
        let mut result = OptimizationResult::NoChange;
        while let Some(node_id) = work.pop() {
            result |= try_eliminate_dead_branch(function, node_id)?;
        }
        Ok(result)
    }
}
