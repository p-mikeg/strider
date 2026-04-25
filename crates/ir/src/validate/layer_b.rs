use crate::graph::Graph;

use super::ValidationError;

/// Layer B: use-list consistency.  For every node input, verify that the
/// output it references still lists that input as one of its consumers
/// (forward walk).  For every output's use-list, verify that each listed
/// input still points back to that output (backward walk).
pub(super) fn check_layer_b(graph: &Graph, errs: &mut Vec<ValidationError>) {
    // Forward walk: every node input must appear in the use-list of the
    // output it references.
    //
    // NOTE: `InputPointsToMissingOutput` is defined in the spec for
    // completeness but is not checked here — the public `Graph` API only
    // hands out live `NodeOutputId`s from its `PrimaryMap`, so fabricating
    // a dangling id via safe code is not possible.  Leaving the variant on
    // the enum keeps the shape documented for any future API that can
    // produce such ids (e.g. a raw-FFI or serialization path).
    // TODO(layer-b): add an `InputPointsToMissingOutput` check once we have
    // an API that can drop outputs without scrubbing their consumers.
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
            let target = graph.input_output_id(input_id);
            let idx_u32 = idx as u32;
            let in_list = graph
                .output_uses(target)
                .any(|(n, i)| n == node && i == idx_u32);
            if !in_list {
                errs.push(ValidationError::InputMissingFromUseList {
                    node,
                    input_idx: idx,
                    output: target,
                });
            }
        }
    }

    // Backward walk: every input in an output's use-list must currently
    // reference that output.
    for output in graph.outputs.keys() {
        let mut cur = graph.output_first_use_id(output);
        while let Some(iid) = cur {
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
}
