use ir::BuiltFunctionGraph;
use ir::node::{NodeId, NodeKind};

use crate::opt::{OptimizationResult, Optimizer};
use crate::utils::{bool_const_val, replace_all_uses};

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
///   `ControlSelector` phi node of that region.
///
/// After this pass, dead `ControlState` nodes end up with zero control inputs
/// and `ControlSelector` phis with a single value input; `RedundantSelectors`
/// then cleans those up.
fn try_eliminate_dead_branch(
    fg: &mut BuiltFunctionGraph,
    node_id: NodeId,
) -> OptimizationResult {
    // Only handle If nodes.
    if !matches!(*fg.graph.node_kind(node_id), NodeKind::If) {
        return OptimizationResult::NoChange;
    }

    // If inputs: [ctrl_in, condition].
    let inputs = fg.graph.node_inputs(node_id);
    if inputs.len() < 2 {
        return OptimizationResult::NoChange;
    }
    let ctrl_in  = inputs[0];
    let cond_out = inputs[1];

    let Some(cond_val) = bool_const_val(fg, cond_out) else {
        return OptimizationResult::NoChange;
    };

    // If outputs: [ctrl_true (index 0), ctrl_false (index 1)].
    let [ctrl_true, ctrl_false] = fg.graph.node_outputs_exact::<2>(node_id);

    let (live_ctrl, dead_ctrl) = if cond_val {
        (ctrl_true, ctrl_false)
    } else {
        (ctrl_false, ctrl_true)
    };

    // ── Step 1: collect dead-ctrl uses before any mutation ────────────────────
    // Each use is (ControlState node, input_index_in_that_node).
    let dead_uses: Vec<(NodeId, u32)> = fg.graph.output_uses(dead_ctrl).collect();

    // ── Step 2: replace live ctrl with ctrl_in (bypass the If) ───────────────
    replace_all_uses(fg, live_ctrl, ctrl_in);

    if dead_uses.is_empty() {
        // No successor for the dead branch — still report Changed if we
        // successfully rerouted the live branch.
        return OptimizationResult::Changed;
    }

    // ── Step 3: remove dead ctrl inputs from successor ControlState(s) ───────
    for (cs_node, dead_idx) in dead_uses {
        if !matches!(*fg.graph.node_kind(cs_node), NodeKind::ControlState) {
            continue; // Unexpected consumer kind; skip safely.
        }

        // ControlState outputs: [ctrl_out, selector_out].
        let cs_outputs = fg.graph.node_outputs(cs_node);
        if cs_outputs.len() < 2 {
            continue;
        }
        let cs_sel_out = cs_outputs[1];

        // Collect ControlSelector phi nodes that consume the selector token
        // before we mutate anything.
        let phi_nodes: Vec<NodeId> = fg
            .graph
            .output_uses(cs_sel_out)
            .map(|(phi, _)| phi)
            .collect();

        // Remove the dead variable-value input from each phi.
        // ControlSelector inputs: [selector_token, val_from_pred0, val_from_pred1, …]
        // So the variable value for predecessor at ControlState index `dead_idx`
        // lives at ControlSelector index `dead_idx + 1`.
        let phi_input_idx = dead_idx + 1;
        for phi_node in phi_nodes {
            let phi_len = fg.graph.node_inputs(phi_node).len() as u32;
            if phi_input_idx < phi_len {
                fg.graph.remove_node_input(phi_node, phi_input_idx);
            }
        }

        // Remove the dead ctrl input from the ControlState itself.
        let cs_len = fg.graph.node_inputs(cs_node).len() as u32;
        if dead_idx < cs_len {
            fg.graph.remove_node_input(cs_node, dead_idx);
        }
    }

    OptimizationResult::Changed
}

// ── Public optimizer ──────────────────────────────────────────────────────────

/// Eliminates branches whose condition is a compile-time boolean constant.
///
/// Works together with [`crate::RedundantSelectors`]: after dead-branch
/// elimination the previously-live successor region may have a single-input
/// `ControlState` and `ControlSelector` phis, which `RedundantSelectors` can
/// then collapse.
pub struct DeadBranchElimination;

impl Optimizer for DeadBranchElimination {
    fn optimize(&self, function: &mut BuiltFunctionGraph) -> OptimizationResult {
        let nodes: Vec<_> = function.preorder().collect();
        let mut result = OptimizationResult::NoChange;
        for node_id in nodes {
            result |= try_eliminate_dead_branch(function, node_id);
        }
        result
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use ir::{FunctionBuilder};
    use ir::node::NodeKind;

    // Helper: count ControlState nodes with N ctrl inputs.
    fn count_cs_with_n_inputs(fg: &ir::BuiltFunctionGraph, n: usize) -> usize {
        fg.all_node_ids()
            .filter(|&node| {
                matches!(fg.graph.node_kind(node), NodeKind::ControlState)
                    && fg.graph.node_inputs(node).len() == n
            })
            .count()
    }

    /// Build a function with `if(cond)`, two branches each ending in `return`.
    /// Returns the built graph and the NodeId of the If node.
    fn make_if_fn(cond_val: bool) -> ir::BuiltFunctionGraph {
        let mut b = FunctionBuilder::new(vec![], &[], &[], &[]);
        let entry = b.create_region();
        let true_region  = b.create_region();
        let false_region = b.create_region();

        b.set_entry_region(entry);
        b.set_region(entry);
        let cond = b.build_boolean_const(cond_val);
        b.build_if(cond, true_region, false_region);

        b.set_region(true_region);
        let true_val = b.build_int_const(1, ir::ValueType::U64);
        b.build_return(Some(true_val), &[]);

        b.set_region(false_region);
        let false_val = b.build_int_const(2, ir::ValueType::U64);
        b.build_return(Some(false_val), &[]);

        b.build()
    }

    #[test]
    fn dead_branch_false() {
        let mut fg = make_if_fn(false);

        // Before: three ControlState nodes with 1 ctrl input each
        // (entry, true-branch, false-branch).
        assert_eq!(count_cs_with_n_inputs(&fg, 1), 3);

        let result = DeadBranchElimination.optimize(&mut fg);
        assert!(result.changed());

        // After: true region's CS loses its input (dead branch removed).
        // Entry CS and false region's CS each still have 1 input.
        assert_eq!(count_cs_with_n_inputs(&fg, 0), 1, "dead branch CS should have 0 inputs");
        assert_eq!(count_cs_with_n_inputs(&fg, 1), 2, "entry and live branch CS should have 1 input");
    }

    #[test]
    fn dead_branch_true() {
        let mut fg = make_if_fn(true);

        assert_eq!(count_cs_with_n_inputs(&fg, 1), 3);

        let result = DeadBranchElimination.optimize(&mut fg);
        assert!(result.changed());

        assert_eq!(count_cs_with_n_inputs(&fg, 0), 1, "dead (false) branch CS should have 0 inputs");
        assert_eq!(count_cs_with_n_inputs(&fg, 1), 2, "entry and live (true) branch CS should have 1 input");
    }

    #[test]
    fn dead_branch_non_const_no_change() {
        // Build if(x) where x is a non-const boolean.
        let mut fg = {
            let mut b = FunctionBuilder::new(vec![], &[], &[], &[]);
            let entry  = b.create_region();
            let true_r = b.create_region();
            let false_r = b.create_region();
            b.set_entry_region(entry);
            b.set_region(entry);
            // Non-constant condition: BoolConst(true) & BoolConst(false)
            // (two nodes combined so it won't be constant at the If level until
            // ConstantFold runs — but we don't run ConstantFold here).
            let t = b.build_boolean_const(true);
            let f = b.build_boolean_const(false);
            let cond = b.build_boolean_operation(t, f, ir::BoolBinaryOp::And);
            b.build_if(cond, true_r, false_r);
            b.set_region(true_r);
            b.build_return(None, &[]);
            b.set_region(false_r);
            b.build_return(None, &[]);
            b.build()
        };

        // DeadBranchElimination alone should not fire because the condition
        // is a BoolBinaryOp node, not a BoolConst.
        assert!(!DeadBranchElimination.optimize(&mut fg).changed());
    }
}
