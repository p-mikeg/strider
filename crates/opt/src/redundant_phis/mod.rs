use rustc_hash::FxHashSet as HashSet;

use crate::error::{ErrorKind, Result};
use crate::pipeline::{OptimizationResult, Optimizer};
use ir::node::NodeId;

/// Replaces all uses of `output` with `value`.  Returns `true` if at least one
/// use was replaced.
fn replace_output_uses(
    function: &mut ir::BuiltFunctionGraph,
    output: ir::node::NodeOutputId,
    value: ir::node::NodeOutputId,
) -> Result<bool> {
    let mut changed = false;
    let mut cursor = function.graph.output_use_cursor(output);
    while cursor.current().is_some() {
        cursor.replace_current_with(value)?;
        changed = true;
    }
    Ok(changed)
}

/// If every output of `node_id` has no uses and the node still has inputs,
/// detaches all inputs (severing dead nodes from the graph) and returns
/// `Changed`.  Otherwise returns `NoChange`.
fn cleanup_if_dead(function: &mut ir::BuiltFunctionGraph, node_id: NodeId) -> OptimizationResult {
    let all_unused = function
        .graph
        .node_outputs(node_id)
        .into_iter()
        .all(|out| function.graph.output_uses(out).next().is_none());

    if all_unused && !function.graph.node_inputs(node_id).is_empty() {
        function.graph.detach_node_inputs(node_id);
        OptimizationResult::Changed
    } else {
        OptimizationResult::NoChange
    }
}

/// Attempts to simplify the phi-like node `node_id` given the set of
/// CFG-reachable nodes.  Returns `Changed` if any transformation was applied.
fn remove_phis(
    function: &mut ir::BuiltFunctionGraph,
    node_id: NodeId,
    reachable: &HashSet<NodeId>,
) -> Result<OptimizationResult> {
    match function.graph.node_kind(node_id) {
        // ControlPhi and MemPhi have identical input layouts after the builder
        // links phi_token as inputs[0] for both:
        //
        //   inputs[0]   = ControlPhi dispatch token from the owning ControlState
        //   inputs[1..] = one value/memory per predecessor, same order as
        //                 ControlState.inputs[0..]
        //
        // Reachability is determined positionally: predecessor j is live iff
        // ControlState.inputs[j]'s producer is in the CFG-reachable set.
        // We deduplicate by NodeOutputId so that two edges from the same
        // predecessor (unusual but valid) count as one.
        ir::node::NodeKind::ControlPhi(..) | ir::node::NodeKind::MemPhi => {
            let inputs = function.graph.node_inputs(node_id);
            if inputs.is_empty() {
                return Ok(OptimizationResult::NoChange);
            }
            let phi_token = inputs[0];
            let control_state_id = function.graph.output_definition(phi_token).0;
            let ctrl_inputs = function.graph.node_inputs(control_state_id);

            let reachable_ctrl: HashSet<ir::node::NodeOutputId> = ctrl_inputs
                .into_iter()
                .filter(|ctrl_in| reachable.contains(&function.graph.output_definition(*ctrl_in).0))
                .collect();

            // Values from live predecessors only: positionally, inputs[j + 1]
            // is the value on predecessor ctrl_inputs[j].
            let live_values: HashSet<ir::node::NodeOutputId> = ctrl_inputs
                .into_iter()
                .enumerate()
                .filter(|&(_j, ctrl_in)| {
                    reachable.contains(&function.graph.output_definition(ctrl_in).0)
                })
                .map(|(j, _ctrl_in)| inputs[j + 1])
                .collect();

            // Drive on iterator-singularity rather than `len()==1`: the
            // `(Some(_), None)` match makes "exactly one element" a
            // structural property the compiler enforces, so we don't need
            // a defensive `ok_or` after the count check.
            let mut ctrl_iter = reachable_ctrl.iter();
            let mut value_iter = live_values.iter();
            let simplified = match (ctrl_iter.next(), ctrl_iter.next()) {
                (Some(&unique_ctrl), None) => {
                    // Find position j such that ctrl_inputs[j] == unique_ctrl, then
                    // take inputs[j + 1] (skipping the phi_token at inputs[0]).
                    let ctrl_inputs2 = function.graph.node_inputs(control_state_id);
                    let Some(j) = ctrl_inputs2.into_iter().position(|c| c == unique_ctrl)
                    else {
                        return Err(ErrorKind::UniqueCtrlNotFound.into());
                    };
                    let value = function.graph.node_inputs(node_id)[j + 1];
                    let [output] = function.graph.node_outputs_exact::<1>(node_id)?;
                    replace_output_uses(function, output, value)?
                }
                _ => match (value_iter.next(), value_iter.next()) {
                    // Distinct live ctrl predecessors all feed the same data
                    // value: the phi is a no-op.  Replace uses with that single
                    // value.  (The ControlState still has multiple real
                    // predecessors, so we don't touch it here.)
                    (Some(&value), None) => {
                        let [output] = function.graph.node_outputs_exact::<1>(node_id)?;
                        replace_output_uses(function, output, value)?
                    }
                    _ => false,
                },
            };

            if simplified {
                function.graph.detach_node_inputs(node_id);
                Ok(OptimizationResult::Changed)
            } else {
                Ok(cleanup_if_dead(function, node_id))
            }
        }
        ir::node::NodeKind::ControlState => {
            let node_inputs = function.graph.node_inputs(node_id);
            let reachable_inputs: HashSet<ir::node::NodeOutputId> = node_inputs
                .into_iter()
                .filter(|inp| reachable.contains(&function.graph.output_definition(*inp).0))
                .collect();

            let mut iter = reachable_inputs.iter();
            let simplified = match (iter.next(), iter.next()) {
                (Some(&input), None) => {
                    let [output, _phi_token] =
                        function.graph.node_outputs_exact::<2>(node_id)?;
                    replace_output_uses(function, output, input)?
                }
                _ => false,
            };

            // For ControlState we can only detach when BOTH outputs are unused.
            // cleanup_if_dead handles this check.
            if simplified {
                Ok(cleanup_if_dead(function, node_id) | OptimizationResult::Changed)
            } else {
                Ok(cleanup_if_dead(function, node_id))
            }
        }
        _ => Ok(OptimizationResult::NoChange),
    }
}

/// Eliminates `ControlPhi`, `MemPhi`, and `ControlState` nodes that have only
/// one reachable predecessor, replacing them with that predecessor's value.
/// Also detaches the inputs of any node that is not reachable from the entry.
///
/// This pass is typically run after [`crate::DeadBranchElimination`], which
/// leaves single-input phis behind.
pub struct RedundantPhis;

impl Optimizer for RedundantPhis {
    fn optimize(&self, function: &mut ir::BuiltFunctionGraph) -> crate::Result<OptimizationResult> {
        let reachable: HashSet<NodeId> =
            ir::walk::cfg_reachable(&function.graph, function.entry)
                .into_iter()
                .collect();
        let mut res = OptimizationResult::NoChange;
        // Only phi-like nodes can be simplified by `remove_phis`, so don't
        // walk every node — pre-filter on the kinds we care about.
        let candidates: Vec<NodeId> = function
            .preorder()
            .filter(|&n| {
                matches!(
                    function.graph.node_kind(n),
                    ir::node::NodeKind::ControlPhi(_)
                        | ir::node::NodeKind::MemPhi
                        | ir::node::NodeKind::ControlState
                )
            })
            .collect();
        for node_id in candidates {
            res |= remove_phis(function, node_id, &reachable)?;
        }
        // Detaching unreachable zombies is bookkeeping, not progress: an
        // unreachable node cannot be a consumer of a reachable producer, so
        // no other pass can act on the result.  Run it for hygiene but do
        // NOT escalate it into a `Changed` signal — that just costs the
        // pipeline one extra fixed-point iteration with no work to do.
        let _ = crate::worklist::detach_unreachable_nodes(function);
        Ok(res)
    }
}

#[cfg(test)]
mod tests;
