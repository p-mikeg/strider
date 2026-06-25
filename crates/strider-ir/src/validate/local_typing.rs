use crate::{
    graph::Graph,
    node::{NodeId, ValueId, ValueKind},
    node_signature::expected_signature,
};

use super::{ValidationError, kind_matches};

/// Local node typing.  For each node, compare its actual input and
/// output [`ValueKind`]s against the [`Signature`] expected for its
/// [`NodeKind`].  For fixed-arity slot lists both arity and each slot kind
/// are checked; for variadic slot lists the head prefix is checked fully,
/// plus every tail index is checked against the repeating tail kind.
pub(super) fn check_local_typing(graph: &Graph, node: NodeId, errs: &mut Vec<ValidationError>) {
    let kind = *graph.node_kind(node);
    let sig = expected_signature(&kind);

    // Most IR nodes have ≤4 inputs/outputs.  Inline up to 4 to skip the heap
    // allocation on the hot validation path; spills transparently for variadic
    // shapes (Call clobber lists, Return arg lists).
    let actual_inputs: smallvec::SmallVec<[ValueId; 4]> =
        graph.node_inputs(node).into_iter().collect();
    let actual_outputs: smallvec::SmallVec<[ValueKind; 4]> = graph
        .node_outputs(node)
        .iter()
        .map(|&oid| graph.value_kind(oid))
        .collect();

    // Arity: fixed lists demand exact length; variadic lists demand at
    // least the head length.  Variadic CTRL lists with `head_len = 0`
    // (e.g. `Region`) trivially pass this check at zero
    // predecessors; the per-kind "must be reachable with >= 1
    // predecessor" rule for those cases is enforced by the
    // graph-invariants checks.
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

    // Kinds: check the head prefix slot-by-slot, then — if the slot list
    // is variadic — check every past-head index against the repeating
    // tail slot. The signature table is the source of truth: tails that
    // need to accept any value type declare AnyValue (or AnyInt for
    // integer-only tails); honest narrow tails like `MemPhi`'s MEM and
    // `Region`'s CTRL are caught here when violated, regardless of
    // what the graph-invariants checks do.
    for (idx, &input) in actual_inputs.iter().enumerate() {
        let Some(slot) = sig.inputs.at(idx) else {
            // Past the head of a fixed-arity list — arity check above
            // already reported a count mismatch.
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
