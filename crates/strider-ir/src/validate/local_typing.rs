use crate::graph::Graph;
use crate::node::signature::expected_signature;
use crate::node::{NodeId, ValueKind};

use super::ValidationError;

/// Checks a node's input/output [`ValueKind`]s against its signature.
pub(super) fn check_local_typing(graph: &Graph, node: NodeId, errs: &mut Vec<ValidationError>) {
    let kind = *graph.node_kind(node);
    let sig = expected_signature(&kind);

    let actual_inputs = graph.node_inputs(node);
    let actual_outputs = graph.node_outputs(node);

    // `Region`'s >= 1 predecessor rule is `check_region`'s.
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

    for (idx, input) in actual_inputs.into_iter().enumerate() {
        let Some(slot) = sig.inputs.at(idx) else {
            // Past a fixed head; the arity check above already reported it.
            break;
        };
        let actual = graph.value_kind(input);
        if !slot.kind.admits(actual) {
            errs.push(ValidationError::NodeInputKindMismatch {
                node,
                input_idx: idx,
                expected: slot.kind,
                actual,
            });
        }
    }

    for (idx, &out) in actual_outputs.iter().enumerate() {
        let actual = graph.value_kind(out);
        if let ValueKind::Typed(ty) = actual
            && !ty.is_supported()
        {
            errs.push(ValidationError::UnsupportedValueType {
                node,
                output_idx: idx,
                ty,
            });
        }
        let Some(slot) = sig.outputs.at(idx) else {
            break;
        };
        if !slot.kind.admits(actual) {
            errs.push(ValidationError::NodeOutputKindMismatch {
                node,
                output_idx: idx,
                expected: slot.kind,
                actual,
            });
        }
    }
}
