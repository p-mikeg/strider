use entity_utils::set::DenseEntitySet;

use crate::graph::Graph;
use crate::node::NodeInputId;
use crate::walk::NodeIdSet;

use super::ValidationError;

/// Use-list consistency.  For every node input, verify that the
/// output it references still lists that input as one of its consumers
/// (forward check).  For every output's use-list, verify that each listed
/// input still points back to that output (backward check).
///
/// Scoped to nodes reachable from the entry — opt passes
/// (`DeadBranchElimination`, `CfgDetach`) detach unreachable
/// subgraphs but leave the zombie nodes in the arena, and re-checking
/// their use-list integrity would surface noise rather than real bugs.
/// `check_local_typing` and `check_graph_invariants_phis` are scoped
/// the same way; this makes the three checks' coverage consistent.
///
/// Implementation: a single sweep over every reachable node's outputs
/// builds a `listed_inputs` set of every `NodeInputId` that currently
/// appears in some use-list, and simultaneously runs the backward
/// consistency check.  The forward check is then a per-input O(1)
/// membership test against that set — total cost O(E) where E is the
/// edge count, vs. the previous O(E·U) "for each edge, scan the
/// target's use-list".
pub(super) fn check_use_list_consistency(
    graph: &Graph,
    reachable: &NodeIdSet,
    errs: &mut Vec<ValidationError>,
) {
    // Single sweep over use-lists, restricted to outputs whose source
    // node is reachable.  Sweeping all outputs would also visit zombie
    // nodes' outputs — those legitimately have empty use-lists, but
    // any consumer in their use-list would itself be a zombie consumer,
    // which we don't want to flag here.
    let mut listed_inputs: DenseEntitySet<NodeInputId> = DenseEntitySet::new();
    for output in graph.outputs.keys() {
        let (source, _) = graph.output_definition(output);
        if !reachable.contains(source) {
            continue;
        }
        let mut cur = graph.output_first_use_id(output);
        while let Some(iid) = cur {
            listed_inputs.insert(iid);
            let referenced = graph.input_output_id(iid);
            if referenced != output {
                errs.push(ValidationError::UseListContainsStaleInput {
                    output,
                    listed_input: iid,
                });
            }
            cur = graph.input_next_use(iid);
        }
    }

    // Forward check: every reachable node's input must appear in some
    // use-list.  Catches the "input was created but never threaded into
    // the producer's use-list" failure mode (covered by the
    // `use_list_input_missing_from_use_list` test, which simulates a
    // corrupted graph via `test_only_clear_first_use`).
    for node in graph.nodes.keys() {
        if !reachable.contains(node) {
            continue;
        }
        let input_count = graph.node_inputs(node).len();
        for idx in 0..input_count {
            // The index range is by construction valid (we just measured the
            // input count); a failure here would be an internal-consistency
            // bug rather than user input. Skip the offending index in the
            // unlikely event of one.
            let Ok(input_id) = graph.node_input_id_at(node, idx) else {
                continue;
            };
            if !listed_inputs.contains(input_id) {
                let target = graph.input_output_id(input_id);
                errs.push(ValidationError::InputMissingFromUseList {
                    node,
                    input_idx: idx,
                    output: target,
                });
            }
        }
    }
}
