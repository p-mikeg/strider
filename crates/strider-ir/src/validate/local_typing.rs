use crate::graph::Graph;
use crate::node::{NodeId, ValueId, ValueKind};
use crate::node_signature::expected_signature;

use super::{ValidationError, kind_matches};

/// Checks a node's input/output [`ValueKind`]s against its signature.
pub(super) fn check_local_typing(graph: &Graph, node: NodeId, errs: &mut Vec<ValidationError>) {
    let kind = *graph.node_kind(node);
    let sig = expected_signature(&kind);

    // Most nodes have <= 4 slots; variadic shapes (Call, Return) spill.
    let actual_inputs: smallvec::SmallVec<[ValueId; 4]> =
        graph.node_inputs(node).into_iter().collect();
    let actual_outputs: smallvec::SmallVec<[ValueKind; 4]> = graph
        .node_outputs(node)
        .iter()
        .map(|&oid| graph.value_kind(oid))
        .collect();

    // A variadic list with `head_len = 0` (e.g. `Region`) passes at zero
    // inputs; the ">= 1 predecessor" rule lives in graph_invariants.
    if let Some(expected) = sig.inputs.arity_violation(actual_inputs.len()) {
        errs.push(ValidationError::NodeInputCountMismatch {
            node,
            expected,
            actual: actual_inputs.len(),
        });
    }

    if let Some(expected) = sig.outputs.arity_violation(actual_outputs.len()) {
        errs.push(ValidationError::NodeOutputCountMismatch {
            node,
            expected,
            actual: actual_outputs.len(),
        });
    }

    for (idx, &input) in actual_inputs.iter().enumerate() {
        let Some(slot) = sig.inputs.at(idx) else {
            // Past a fixed head; the arity check above already reported it.
            break;
        };
        let actual = graph.value_kind(input);
        if !kind_matches(slot.kind, actual) {
            errs.push(ValidationError::NodeInputKindMismatch {
                node,
                input_idx: idx,
                expected: slot.kind,
                actual,
            });
        }
    }

    for (idx, &actual) in actual_outputs.iter().enumerate() {
        let Some(slot) = sig.outputs.at(idx) else {
            break;
        };
        if !kind_matches(slot.kind, actual) {
            errs.push(ValidationError::NodeOutputKindMismatch {
                node,
                output_idx: idx,
                expected: slot.kind,
                actual,
            });
        }
    }
}
