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
use crate::node::{NodeId, NodeInputId, NodeKind, NodeOutputId, NodeOutputKind, NodeOutputType};
use crate::node_signature::{expected_signature, ExpectedOutputKind};

/// Validates the structural invariants of `graph` starting from `entry`.
///
/// Returns `Ok(())` if every checked invariant holds, or a
/// [`ValidationErrors`] bundle describing every violation otherwise.
pub fn validate(graph: &Graph, entry: NodeId) -> Result<(), ValidationErrors> {
    let mut errs: Vec<ValidationError> = Vec::new();

    for node in graph.nodes.keys() {
        check_layer_a(graph, node, &mut errs);
    }

    check_layer_b(graph, &mut errs);

    check_layer_c_uniqueness(graph, &mut errs);

    check_layer_c_control_state(graph, &mut errs);

    check_layer_c_phis(graph, &mut errs);

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
        if !kind_matches(expected_inputs[idx], actual) {
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
        if !kind_matches(expected_outputs[idx], actual_outputs[idx]) {
            errs.push(ValidationError::NodeOutputKindMismatch {
                node,
                output_idx: idx,
                expected: expected_outputs[idx],
                actual: actual_outputs[idx],
            });
        }
    }
}

/// Layer B: use-list consistency.  For every node input, verify that the
/// output it references still lists that input as one of its consumers
/// (forward walk).  For every output's use-list, verify that each listed
/// input still points back to that output (backward walk).
fn check_layer_b(graph: &Graph, errs: &mut Vec<ValidationError>) {
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
            let input_id = graph.node_input_id_at(node, idx);
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

/// Layer C (shape check): enforce that the graph has exactly one
/// [`NodeKind::Entry`] node and exactly one [`NodeKind::InitialMemory`] node.
///
/// Emits [`ValidationError::MissingEntryNode`] /
/// [`ValidationError::MissingInitialMemoryNode`] when a kind is absent, and
/// [`ValidationError::MultipleEntryNodes`] /
/// [`ValidationError::MultipleInitialMemoryNodes`] (carrying the first two
/// offenders) when a kind appears more than once.
fn check_layer_c_uniqueness(graph: &Graph, errs: &mut Vec<ValidationError>) {
    let mut entries: Vec<NodeId> = Vec::new();
    let mut initial_memories: Vec<NodeId> = Vec::new();

    for node in graph.nodes.keys() {
        match graph.node_kind(node) {
            NodeKind::Entry => entries.push(node),
            NodeKind::InitialMemory => initial_memories.push(node),
            _ => {}
        }
    }

    match entries.as_slice() {
        [] => errs.push(ValidationError::MissingEntryNode),
        [_] => {}
        [first, second, ..] => errs.push(ValidationError::MultipleEntryNodes {
            first: *first,
            second: *second,
        }),
    }

    match initial_memories.as_slice() {
        [] => errs.push(ValidationError::MissingInitialMemoryNode),
        [_] => {}
        [first, second, ..] => errs.push(ValidationError::MultipleInitialMemoryNodes {
            first: *first,
            second: *second,
        }),
    }
}

/// Layer C: every input of a `ControlState` node must be a `Control`-kinded
/// output. Emits `ControlStateNonControlPredecessor` per offending input.
fn check_layer_c_control_state(graph: &Graph, errs: &mut Vec<ValidationError>) {
    for node in graph.nodes.keys() {
        if !matches!(graph.node_kind(node), NodeKind::ControlState) {
            continue;
        }
        for (idx, target) in graph.node_inputs(node).into_iter().enumerate() {
            let kind = graph.output_kind(target);
            if kind != NodeOutputKind::Control {
                let (producer, _) = graph.output_definition(target);
                errs.push(ValidationError::ControlStateNonControlPredecessor {
                    control_state: node,
                    input_idx: idx,
                    producer,
                    producer_kind: kind,
                });
            }
        }
    }
}

/// Layer C: every phi node (`ControlPhi`, `MemPhi`, `StackStorePhi`) must take
/// its dispatch token (input[0]) from a `ControlState`'s `ControlPhi` output,
/// and the number of value inputs must match the owning `ControlState`'s
/// predecessor count.
fn check_layer_c_phis(graph: &Graph, errs: &mut Vec<ValidationError>) {
    for node in graph.nodes.keys() {
        let is_phi = matches!(
            graph.node_kind(node),
            NodeKind::ControlPhi(_) | NodeKind::MemPhi | NodeKind::StackStorePhi { .. }
        );
        if !is_phi {
            continue;
        }

        let inputs: Vec<NodeOutputId> = graph.node_inputs(node).into_iter().collect();
        if inputs.is_empty() {
            continue; // Layer A already flagged this.
        }
        let token = inputs[0];
        let token_kind = graph.output_kind(token);
        if token_kind != NodeOutputKind::ControlPhi {
            let (producer, _) = graph.output_definition(token);
            errs.push(ValidationError::PhiTokenNotFromControlState {
                phi: node,
                producer,
                producer_kind: token_kind,
            });
            continue;
        }

        let (owner, _idx) = graph.output_definition(token);
        if !matches!(graph.node_kind(owner), NodeKind::ControlState) {
            errs.push(ValidationError::PhiTokenNotFromControlState {
                phi: node,
                producer: owner,
                producer_kind: token_kind,
            });
            continue;
        }

        let expected_preds = graph.node_inputs(owner).into_iter().count();
        let actual_values = inputs.len() - 1;
        if expected_preds != actual_values {
            errs.push(ValidationError::PhiValueArityMismatch {
                phi: node,
                owner_control_state: owner,
                expected_predecessors: expected_preds,
                actual_values,
            });
        }
    }
}

/// Returns whether an actual [`NodeOutputKind`] satisfies the
/// [`ExpectedOutputKind`] declared by a [`NodeKind`]'s signature.
///
/// `AnyInt` matches any integer-typed output (U8, U16, U32, U64, U128, U256);
/// `AnyFloat` matches F32 or F64; `Bool` matches only `OutputType(Bool)`.
/// `Control`, `Memory`, and `ControlPhi` match their identically-named
/// [`NodeOutputKind`] variants.
fn kind_matches(expected: ExpectedOutputKind, actual: NodeOutputKind) -> bool {
    match (expected, actual) {
        (ExpectedOutputKind::Control, NodeOutputKind::Control) => true,
        (ExpectedOutputKind::Memory, NodeOutputKind::Memory) => true,
        (ExpectedOutputKind::ControlPhi, NodeOutputKind::ControlPhi) => true,
        (ExpectedOutputKind::Bool, NodeOutputKind::OutputType(NodeOutputType::Bool)) => true,
        (ExpectedOutputKind::AnyInt, NodeOutputKind::OutputType(t)) if t.is_integer() => true,
        (ExpectedOutputKind::AnyFloat, NodeOutputKind::OutputType(t)) if t.is_float() => true,
        _ => false,
    }
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
        expected: ExpectedOutputKind,
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
        expected: ExpectedOutputKind,
        actual: NodeOutputKind,
    },

    #[error("node {node:?} input[{input_idx}] references missing output {output:?}")]
    InputPointsToMissingOutput {
        node: NodeId,
        input_idx: usize,
        output: NodeOutputId,
    },

    #[error(
        "node {node:?} input[{input_idx}] references output {output:?} \
         but is not in that output's use-list"
    )]
    InputMissingFromUseList {
        node: NodeId,
        input_idx: usize,
        output: NodeOutputId,
    },

    #[error(
        "output {output:?}'s use-list contains input {listed_input:?} \
         that no longer references this output"
    )]
    UseListContainsStaleInput {
        output: NodeOutputId,
        listed_input: NodeInputId,
    },

    #[error("multiple Entry nodes: {first:?} and {second:?}")]
    MultipleEntryNodes { first: NodeId, second: NodeId },

    #[error("multiple InitialMemory nodes: {first:?} and {second:?}")]
    MultipleInitialMemoryNodes { first: NodeId, second: NodeId },

    #[error("missing Entry node")]
    MissingEntryNode,

    #[error("missing InitialMemory node")]
    MissingInitialMemoryNode,

    #[error(
        "ControlState {control_state:?} input[{input_idx}] producer {producer:?} \
         has kind {producer_kind:?}, expected Control"
    )]
    ControlStateNonControlPredecessor {
        control_state: NodeId,
        input_idx: usize,
        producer: NodeId,
        producer_kind: NodeOutputKind,
    },

    #[error(
        "phi node {phi:?} input[0] token producer {producer:?} has kind \
         {producer_kind:?}; expected ControlPhi from a ControlState"
    )]
    PhiTokenNotFromControlState {
        phi: NodeId,
        producer: NodeId,
        producer_kind: NodeOutputKind,
    },

    #[error(
        "phi {phi:?} has {actual_values} value inputs but its ControlState \
         owner {owner_control_state:?} has {expected_predecessors} predecessors"
    )]
    PhiValueArityMismatch {
        phi: NodeId,
        owner_control_state: NodeId,
        expected_predecessors: usize,
        actual_values: usize,
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
    use crate::node::{NodeKind, NodeOutputKind, NodeOutputType};

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
    fn layer_b_input_missing_from_use_list() {
        use crate::node::NodeOutputType;
        use crate::ops::IntUnaryOp;

        let mut graph = Graph::new();
        let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
        let _mem = graph.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory]);

        let c = graph.create_node(
            NodeKind::IntConst(3),
            [],
            [NodeOutputKind::OutputType(NodeOutputType::U64)],
        );
        let c_out = graph.node_outputs(c).into_iter().next().unwrap();

        let _neg = graph.create_node(
            NodeKind::IntUnaryOp(IntUnaryOp::Neg),
            [c_out],
            [NodeOutputKind::OutputType(NodeOutputType::U64)],
        );

        // Corrupt the forward link: clear the IntConst output's head-of-use
        // pointer.  The op's input is still recorded, but the producer no
        // longer admits it as a consumer.
        graph.test_only_clear_first_use(c_out);

        let errs = validate(&graph, entry).unwrap_err();
        assert!(
            errs.0.iter().any(|e| matches!(
                e,
                ValidationError::InputMissingFromUseList { input_idx: 0, .. }
            )),
            "expected InputMissingFromUseList, got: {errs:?}"
        );
    }

    #[test]
    fn layer_b_stale_input_in_use_list() {
        use crate::node::NodeOutputType;
        use crate::ops::IntUnaryOp;

        let mut graph = Graph::new();
        let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
        let _mem = graph.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory]);

        let a = graph.create_node(
            NodeKind::IntConst(1),
            [],
            [NodeOutputKind::OutputType(NodeOutputType::U64)],
        );
        let a_out = graph.node_outputs(a).into_iter().next().unwrap();

        let b = graph.create_node(
            NodeKind::IntConst(2),
            [],
            [NodeOutputKind::OutputType(NodeOutputType::U64)],
        );
        let b_out = graph.node_outputs(b).into_iter().next().unwrap();

        let neg = graph.create_node(
            NodeKind::IntUnaryOp(IntUnaryOp::Neg),
            [a_out],
            [NodeOutputKind::OutputType(NodeOutputType::U64)],
        );

        // Retarget the op's input at idx 0 to `b_out` without updating any
        // use-list.  `a_out`'s use-list still references this input, but the
        // input itself now points at `b_out` — that's a stale entry.
        let input_id = graph.node_input_id_at(neg, 0);
        graph.test_only_retarget_input(input_id, b_out);

        let errs = validate(&graph, entry).unwrap_err();
        assert!(
            errs.0.iter().any(|e| matches!(
                e,
                ValidationError::UseListContainsStaleInput { .. }
            )),
            "expected UseListContainsStaleInput, got: {errs:?}"
        );
    }

    #[test]
    fn layer_c_missing_initial_memory() {
        let mut graph = Graph::new();
        let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);

        let errs = validate(&graph, entry).unwrap_err();
        assert!(
            errs.0
                .iter()
                .any(|e| matches!(e, ValidationError::MissingInitialMemoryNode)),
            "expected MissingInitialMemoryNode, got: {errs:?}"
        );
    }

    #[test]
    fn layer_c_duplicate_entry() {
        let mut graph = Graph::new();
        let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
        let _entry2 = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
        let _mem = graph.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory]);

        let errs = validate(&graph, entry).unwrap_err();
        assert!(
            errs.0
                .iter()
                .any(|e| matches!(e, ValidationError::MultipleEntryNodes { .. })),
            "expected MultipleEntryNodes, got: {errs:?}"
        );
    }

    #[test]
    fn layer_c_duplicate_initial_memory() {
        let mut graph = Graph::new();
        let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
        let _mem1 = graph.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory]);
        let _mem2 = graph.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory]);

        let errs = validate(&graph, entry).unwrap_err();
        assert!(
            errs.0
                .iter()
                .any(|e| matches!(e, ValidationError::MultipleInitialMemoryNodes { .. })),
            "expected MultipleInitialMemoryNodes, got: {errs:?}"
        );
    }

    #[test]
    fn layer_c_control_state_bad_predecessor() {
        let mut graph = Graph::new();
        let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
        let mem = graph.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory]);
        let mem_out = graph.node_outputs(mem).into_iter().next().unwrap();

        // ControlState with a Memory predecessor instead of Control.
        let _bad_cs = graph.create_node(
            NodeKind::ControlState,
            [mem_out],
            [NodeOutputKind::Control, NodeOutputKind::ControlPhi],
        );

        let errs = validate(&graph, entry).unwrap_err();
        assert!(errs.0.iter().any(|e| matches!(
            e,
            ValidationError::ControlStateNonControlPredecessor { input_idx: 0, .. }
        )), "got: {errs:?}");
    }

    fn test_vn() -> rsleigh::Vn {
        rsleigh::Vn {
            addr: rsleigh::VnAddr {
                space: rsleigh::VnSpace::REGISTER,
                off: 0x20,
            },
            size: 4,
        }
    }

    #[test]
    fn layer_c_phi_token_from_wrong_node() {
        let mut graph = Graph::new();
        let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
        let _mem = graph.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory]);
        let entry_out = graph.node_outputs(entry).into_iter().next().unwrap();
        let cs = graph.create_node(
            NodeKind::ControlState,
            [entry_out],
            [NodeOutputKind::Control, NodeOutputKind::ControlPhi],
        );
        let cs_control_out = graph.node_outputs(cs).into_iter().next().unwrap(); // index 0 = Control
        let vn = test_vn();
        let _phi = graph.create_node(
            NodeKind::ControlPhi(vn),
            [cs_control_out],
            [NodeOutputKind::OutputType(NodeOutputType::U64)],
        );

        let errs = validate(&graph, entry).unwrap_err();
        assert!(errs.0.iter().any(|e| matches!(
            e,
            ValidationError::PhiTokenNotFromControlState { .. }
        )), "got: {errs:?}");
    }

    #[test]
    fn layer_c_phi_value_arity_mismatch() {
        let mut graph = Graph::new();
        let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
        let _mem = graph.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory]);
        let entry_out = graph.node_outputs(entry).into_iter().next().unwrap();

        let cs = graph.create_node(
            NodeKind::ControlState,
            [entry_out],
            [NodeOutputKind::Control, NodeOutputKind::ControlPhi],
        );
        let cs_phi_out = graph.node_outputs(cs).into_iter().nth(1).unwrap();

        let c1 = graph.create_node(NodeKind::IntConst(1), [], [NodeOutputKind::OutputType(NodeOutputType::U64)]);
        let c2 = graph.create_node(NodeKind::IntConst(2), [], [NodeOutputKind::OutputType(NodeOutputType::U64)]);
        let c1_out = graph.node_outputs(c1).into_iter().next().unwrap();
        let c2_out = graph.node_outputs(c2).into_iter().next().unwrap();
        let vn = test_vn();
        let _phi = graph.create_node(
            NodeKind::ControlPhi(vn),
            [cs_phi_out, c1_out, c2_out],
            [NodeOutputKind::OutputType(NodeOutputType::U64)],
        );

        let errs = validate(&graph, entry).unwrap_err();
        assert!(errs.0.iter().any(|e| matches!(
            e,
            ValidationError::PhiValueArityMismatch { expected_predecessors: 1, actual_values: 2, .. }
        )), "got: {errs:?}");
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
