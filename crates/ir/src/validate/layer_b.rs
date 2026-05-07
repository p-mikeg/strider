use entity_utils::set::DenseEntitySet;

use crate::graph::Graph;
use crate::node::NodeInputId;

use super::ValidationError;

/// Layer B: use-list consistency.  For every node input, verify that the
/// output it references still lists that input as one of its consumers
/// (forward check).  For every output's use-list, verify that each listed
/// input still points back to that output (backward check).
///
/// Implementation: a single sweep over every output's use-list builds a
/// `listed_inputs` set of every `NodeInputId` that currently appears in
/// some use-list, and simultaneously runs the backward consistency check.
/// The forward check is then a per-input O(1) membership test against
/// that set — total cost O(E) where E is the edge count, vs. the
/// previous O(E·U) "for each edge, scan the target's use-list".
pub(super) fn check_layer_b(graph: &Graph, errs: &mut Vec<ValidationError>) {
    // Single sweep over use-lists: collect every listed input id and
    // catch backward inconsistency in one pass.
    let mut listed_inputs: DenseEntitySet<NodeInputId> = DenseEntitySet::new();
    for output in graph.outputs.keys() {
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

    // Forward check: every node input must appear in some use-list.
    // Catches the "input was created but never threaded into the
    // producer's use-list" failure mode (covered by the
    // `layer_b_input_missing_from_use_list` test, which simulates a
    // corrupted graph via `test_only_clear_first_use`).
    for node in graph.nodes.keys() {
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
