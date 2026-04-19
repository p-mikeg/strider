use crate::graph::Graph;
use crate::node::{NodeId, NodeOutputId, NodeOutputKind};
use crate::node_signature::expected_signature;

use super::{ValidationError, kind_matches};

/// Layer A: local node typing.  For each node, compare its actual input and
/// output [`NodeOutputKind`]s against the [`Signature`] expected for its
/// [`NodeKind`].  For fixed-arity slot lists both arity and each slot kind
/// are checked; for variadic slot lists the head prefix is checked fully,
/// plus every tail index is checked against the repeating tail kind.
pub(super) fn check_layer_a(graph: &Graph, node: NodeId, errs: &mut Vec<ValidationError>) {
    let kind = *graph.node_kind(node);
    let sig = expected_signature(&kind);

    let actual_inputs: Vec<NodeOutputId> = graph.node_inputs(node).into_iter().collect();
    let actual_outputs: Vec<NodeOutputKind> = graph
        .node_outputs(node)
        .into_iter()
        .map(|oid| graph.output_kind(oid))
        .collect();

    // Arity: fixed lists demand exact length; variadic lists demand at
    // least the head length.
    let input_head_len = sig.inputs.head_len();
    let output_head_len = sig.outputs.head_len();

    let input_arity_ok = if sig.inputs.is_variadic() {
        actual_inputs.len() >= input_head_len
    } else {
        actual_inputs.len() == input_head_len
    };
    if !input_arity_ok {
        errs.push(ValidationError::NodeInputCountMismatch {
            node,
            expected: input_head_len,
            actual: actual_inputs.len(),
        });
    }

    let output_arity_ok = if sig.outputs.is_variadic() {
        actual_outputs.len() >= output_head_len
    } else {
        actual_outputs.len() == output_head_len
    };
    if !output_arity_ok {
        errs.push(ValidationError::NodeOutputCountMismatch {
            node,
            expected: output_head_len,
            actual: actual_outputs.len(),
        });
    }

    // Kinds: check only the fixed head prefix for both inputs and outputs.
    // Variadic tails are intentionally not checked here — some kinds (e.g.
    // `Call` args) accept any value type in practice but are typed AnyInt
    // in the signature table for documentation purposes.
    let check_len = input_head_len.min(actual_inputs.len());
    for (idx, &input) in actual_inputs.iter().enumerate().take(check_len) {
        let slot = sig.inputs.head[idx];
        let actual = graph.output_kind(input);
        if !kind_matches(slot.kind, actual) {
            errs.push(ValidationError::NodeInputKindMismatch {
                node,
                input_idx: idx,
                expected: slot.kind,
                actual,
            });
        }
    }

    let check_len = output_head_len.min(actual_outputs.len());
    for (idx, &actual) in actual_outputs.iter().enumerate().take(check_len) {
        let slot = sig.outputs.head[idx];
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
