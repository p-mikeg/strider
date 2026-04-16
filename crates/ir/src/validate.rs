//! Whole-graph validator for the IR.
//!
//! The validator walks a built [`Graph`] starting from an entry [`NodeId`] and
//! checks structural invariants (signatures, reachability, use-list
//! consistency, etc.).  This module currently contains only the skeleton;
//! concrete checks are added by later tasks.
//!
//! On failure the validator returns a [`ValidationErrors`] bundle that
//! aggregates every [`ValidationError`] it found during a single pass, so
//! callers can see all problems at once rather than only the first.

use crate::graph::Graph;
use crate::node::{NodeId, NodeKind, NodeOutputId, NodeOutputKind};
use crate::node_signature::expected_signature;

/// Validates the structural invariants of `graph` starting from `entry`.
///
/// Returns `Ok(())` if every checked invariant holds, or a
/// [`ValidationErrors`] bundle describing every violation otherwise.
pub fn validate(graph: &Graph, entry: NodeId) -> Result<(), ValidationErrors> {
    let mut errs: Vec<ValidationError> = Vec::new();

    for node in graph.nodes.keys() {
        check_layer_a(graph, node, &mut errs);
    }

    let _ = entry;

    if errs.is_empty() {
        Ok(())
    } else {
        Err(ValidationErrors(errs))
    }
}

/// Layer A: local node typing.  For each node, compare its actual input and
/// output [`NodeOutputKind`]s against the signature expected for its
/// [`NodeKind`].  Variadic kinds only have their fixed prefix checked here;
/// the variadic tails are validated by later layers.
fn check_layer_a(graph: &Graph, node: NodeId, errs: &mut Vec<ValidationError>) {
    let kind = *graph.node_kind(node);
    let (expected_inputs, expected_outputs) = expected_signature(&kind);

    let actual_inputs: Vec<NodeOutputId> = graph.node_inputs(node).into_iter().collect();
    let actual_outputs: Vec<NodeOutputKind> = graph
        .node_outputs(node)
        .into_iter()
        .map(|oid| graph.output_kind(oid))
        .collect();

    // Variadic kinds: only the fixed prefix of inputs is checked; the rest is
    // validated by Layer C for the kinds that need it.
    let is_variadic_input = matches!(
        kind,
        NodeKind::ControlState
            | NodeKind::MemPhi
            | NodeKind::ControlPhi(_)
            | NodeKind::Call
            | NodeKind::Return
            | NodeKind::CallOther { .. }
            | NodeKind::New
            | NodeKind::CPoolRef
    );
    let is_variadic_output = matches!(kind, NodeKind::Call | NodeKind::CallOther { .. });

    if !is_variadic_input && actual_inputs.len() != expected_inputs.len() {
        errs.push(ValidationError::NodeInputCountMismatch {
            node,
            expected: expected_inputs.len(),
            actual: actual_inputs.len(),
        });
    }
    if !is_variadic_output && actual_outputs.len() != expected_outputs.len() {
        errs.push(ValidationError::NodeOutputCountMismatch {
            node,
            expected: expected_outputs.len(),
            actual: actual_outputs.len(),
        });
    }

    // Check the fixed prefix of input kinds.
    let check_len = expected_inputs.len().min(actual_inputs.len());
    for idx in 0..check_len {
        let actual = graph.output_kind(actual_inputs[idx]);
        if !kinds_compatible(expected_inputs[idx], actual) {
            errs.push(ValidationError::NodeInputKindMismatch {
                node,
                input_idx: idx,
                expected: expected_inputs[idx],
                actual,
            });
        }
    }

    // Check the fixed prefix of output kinds.
    let check_len = expected_outputs.len().min(actual_outputs.len());
    for idx in 0..check_len {
        if !kinds_compatible(expected_outputs[idx], actual_outputs[idx]) {
            errs.push(ValidationError::NodeOutputKindMismatch {
                node,
                output_idx: idx,
                expected: expected_outputs[idx],
                actual: actual_outputs[idx],
            });
        }
    }
}

/// Two `NodeOutputKind`s are compatible if they are the same variant.
/// For `OutputType`, any integer width is compatible with any other (width
/// checks are not part of this layer).
fn kinds_compatible(expected: NodeOutputKind, actual: NodeOutputKind) -> bool {
    use NodeOutputKind::*;
    matches!(
        (expected, actual),
        (Control, Control)
            | (Memory, Memory)
            | (ControlPhi, ControlPhi)
            | (OutputType(_), OutputType(_)),
    )
}

/// A bundle of [`ValidationError`]s produced by a single [`validate`] call.
pub struct ValidationErrors(pub Vec<ValidationError>);

/// An individual IR validation failure.
#[derive(Debug, thiserror::Error)]
pub enum ValidationError {
    #[error("node {node:?} has {actual} inputs, expected {expected}")]
    NodeInputCountMismatch {
        node: NodeId,
        expected: usize,
        actual: usize,
    },

    #[error("node {node:?} input[{input_idx}] has kind {actual:?}, expected {expected:?}")]
    NodeInputKindMismatch {
        node: NodeId,
        input_idx: usize,
        expected: NodeOutputKind,
        actual: NodeOutputKind,
    },

    #[error("node {node:?} has {actual} outputs, expected {expected}")]
    NodeOutputCountMismatch {
        node: NodeId,
        expected: usize,
        actual: usize,
    },

    #[error("node {node:?} output[{output_idx}] has kind {actual:?}, expected {expected:?}")]
    NodeOutputKindMismatch {
        node: NodeId,
        output_idx: usize,
        expected: NodeOutputKind,
        actual: NodeOutputKind,
    },
}

impl std::fmt::Debug for ValidationErrors {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("ValidationErrors").field(&self.0).finish()
    }
}

impl std::fmt::Display for ValidationErrors {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for err in &self.0 {
            writeln!(f, "{err}")?;
        }
        Ok(())
    }
}

impl std::error::Error for ValidationErrors {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::{NodeKind, NodeOutputKind};

    #[test]
    fn empty_graph_with_entry_only() {
        let mut graph = Graph::new();
        let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
        let _mem = graph.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory]);
        assert!(validate(&graph, entry).is_ok());
    }

    #[test]
    fn layer_a_wrong_input_kind_on_int_unary_op() {
        use crate::node::NodeOutputType;
        use crate::ops::IntUnaryOp;

        let mut graph = Graph::new();
        let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
        let _mem = graph.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory]);

        // IntUnaryOp expects an OutputType input, but we feed it a Control output.
        let control_out = graph.node_outputs(entry).into_iter().next().unwrap();
        let _bad = graph.create_node(
            NodeKind::IntUnaryOp(IntUnaryOp::Neg),
            [control_out],
            [NodeOutputKind::OutputType(NodeOutputType::U64)],
        );

        let errs = validate(&graph, entry).unwrap_err();
        assert!(
            errs.0.iter().any(|e| matches!(
                e,
                ValidationError::NodeInputKindMismatch { input_idx: 0, .. }
            )),
            "expected a NodeInputKindMismatch, got: {errs:?}"
        );
    }

    #[test]
    fn layer_a_wrong_output_kind() {
        let mut graph = Graph::new();
        // Entry should produce Control, we make it produce Memory instead.
        let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Memory]);
        let _mem = graph.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory]);

        let errs = validate(&graph, entry).unwrap_err();
        assert!(
            errs.0.iter().any(|e| matches!(
                e,
                ValidationError::NodeOutputKindMismatch { output_idx: 0, .. }
            )),
            "got: {errs:?}"
        );
    }

    #[test]
    fn layer_a_wrong_input_count() {
        use crate::node::NodeOutputType;
        use crate::ops::IntBinaryOp;

        let mut graph = Graph::new();
        let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
        let _mem = graph.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory]);
        let c = graph.create_node(
            NodeKind::IntConst(5),
            [],
            [NodeOutputKind::OutputType(NodeOutputType::U64)],
        );
        let c_out = graph.node_outputs(c).into_iter().next().unwrap();

        // IntBinaryOp expects 2 inputs; give it 1.
        let _bad = graph.create_node(
            NodeKind::IntBinaryOp(IntBinaryOp::Add),
            [c_out],
            [NodeOutputKind::OutputType(NodeOutputType::U64)],
        );

        let errs = validate(&graph, entry).unwrap_err();
        assert!(
            errs.0.iter().any(|e| matches!(
                e,
                ValidationError::NodeInputCountMismatch {
                    expected: 2,
                    actual: 1,
                    ..
                }
            )),
            "got: {errs:?}"
        );
    }
}
