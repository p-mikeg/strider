use crate::graph::Graph;
use crate::node::{NodeId, ValueId, ValueKind};
use crate::node_signature::expected_signature;

use super::{ValidationError, kind_matches};

/// Checks a node's input/output [`ValueKind`]s against its signature.
///
/// Fixed-arity slot lists are checked for exact length and per-slot kind;
/// variadic lists check the head prefix fully, then every past-head index
/// against the repeating tail kind.
pub(super) fn check_local_typing(graph: &Graph, node: NodeId, errs: &mut Vec<ValidationError>) {
    let kind = *graph.node_kind(node);
    let sig = expected_signature(&kind);

    // Most nodes have <= 4 slots; inlining skips the allocation on this hot
    // path and spills for variadic shapes (Call clobbers, Return args).
    let actual_inputs: smallvec::SmallVec<[ValueId; 4]> =
        graph.node_inputs(node).into_iter().collect();
    let actual_outputs: smallvec::SmallVec<[ValueKind; 4]> = graph
        .node_outputs(node)
        .iter()
        .map(|&oid| graph.value_kind(oid))
        .collect();

    // A variadic list with `head_len = 0` (e.g. `Region`) passes trivially at
    // zero inputs; the ">= 1 predecessor" rule lives in graph_invariants.
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

    // The signature table is the source of truth for tail kinds: permissive
    // tails declare AnyValue / AnyInt, so narrow ones (`MemPhi`'s MEM,
    // `Region`'s CTRL) are genuinely enforced here.
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
