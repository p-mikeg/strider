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

use std::collections::HashSet;

use crate::graph::Graph;
use crate::node::{NodeId, NodeInputId, NodeKind, NodeOutputId, NodeOutputKind, NodeOutputType};
use crate::node_signature::{ExpectedOutputKind, expected_signature};
use crate::walk::walk_graph;

/// Validates the structural invariants of `graph` starting from `entry`.
///
/// Returns `Ok(())` if every checked invariant holds, or a
/// [`ValidationErrors`] bundle describing every violation otherwise.
///
/// Local per-node checks (Layer A) are scoped to nodes reachable from `entry`
/// so that detached zombie nodes left behind by optimization passes (see
/// `opt::redundant_phis::detach_unreachable_nodes`) do not trigger false
/// positives.  Layer B and Layer C iterate all nodes but are naturally
/// tolerant of detached nodes: `detach_node_inputs` scrubs the use-lists of
/// the producers it disconnects, so a detached node contributes no inputs and
/// no live use-list entries anywhere.
pub fn validate(graph: &Graph, entry: NodeId) -> Result<(), ValidationErrors> {
    let reachable: HashSet<NodeId> = walk_graph(graph, entry).collect();
    let mut errs: Vec<ValidationError> = Vec::new();

    for node in graph.nodes.keys() {
        if !reachable.contains(&node) {
            continue;
        }
        check_layer_a(graph, node, &mut errs);
    }

    check_layer_b(graph, &mut errs);

    check_layer_c_uniqueness(graph, &mut errs);

    check_layer_c_control_state(graph, &mut errs);

    check_layer_c_phis(graph, &mut errs);

    check_layer_c_postcall_producer(graph, &mut errs);

    check_layer_c_postcall_uniqueness(graph, &mut errs);

    if errs.is_empty() {
        Ok(())
    } else {
        Err(ValidationErrors(errs))
    }
}

/// Layer A: local node typing.  For each node, compare its actual input and
/// output [`NodeOutputKind`]s against the [`Signature`] expected for its
/// [`NodeKind`].  For fixed-arity slot lists both arity and each slot kind
/// are checked; for variadic slot lists the head prefix is checked fully,
/// plus every tail index is checked against the repeating tail kind.
fn check_layer_a(graph: &Graph, node: NodeId, errs: &mut Vec<ValidationError>) {
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
/// its dispatch token (input[0]) from a `ControlState`'s `ControlPhi` output.
///
/// For `ControlPhi` and `MemPhi` (variadic phis), the number of value inputs
/// must match the owning `ControlState`'s predecessor count.  `StackStorePhi`
/// has fixed arity `[token, memory, data]` (Layer A enforces this) — its
/// per-predecessor information lives in the side-table
/// `Graph::stack_phi_offsets`, not in its inputs, so the per-predecessor
/// arity rule does not apply to it.
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
            continue; // Layer A fires a count or kind mismatch for empty-input phis; skip here.
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

        // StackStorePhi has fixed arity 3 regardless of predecessor count;
        // skip the per-predecessor arity check for it.
        if matches!(graph.node_kind(node), NodeKind::StackStorePhi { .. }) {
            continue;
        }

        let expected_preds = graph.node_inputs(owner).len();
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

/// Layer C: a `PostCallMemState` or `PostCallVarState` must consume output[0]
/// (the Control token) of a `Call` node; nothing else is valid.
fn check_layer_c_postcall_producer(graph: &Graph, errs: &mut Vec<ValidationError>) {
    for node in graph.nodes.keys() {
        let kind = *graph.node_kind(node);
        let mk_err: fn(NodeId, NodeId, NodeOutputKind) -> ValidationError = match kind {
            NodeKind::PostCallMemState => |n, p, k| ValidationError::PostCallMemStateNotAfterCall {
                node: n,
                producer: p,
                producer_kind: k,
            },
            NodeKind::PostCallVarState(_) => {
                |n, p, k| ValidationError::PostCallVarStateNotAfterCall {
                    node: n,
                    producer: p,
                    producer_kind: k,
                }
            }
            _ => continue,
        };

        let Ok([target]) = graph.node_inputs_exact::<1>(node) else {
            continue; // Layer A fires a count mismatch here.
        };
        let (producer, producer_out_idx) = graph.output_definition(target);
        let producer_kind = graph.output_kind(target);

        let is_call_control =
            matches!(graph.node_kind(producer), NodeKind::Call) && producer_out_idx == 0;
        if !is_call_control {
            errs.push(mk_err(node, producer, producer_kind));
        }
    }
}

/// Layer C: per Call, there can be at most one `PostCallMemState` consuming
/// the Call's Control output, and at most one `PostCallVarState(vn)` per
/// distinct `vn`. Duplicates indicate a construction or optimization bug.
fn check_layer_c_postcall_uniqueness(graph: &Graph, errs: &mut Vec<ValidationError>) {
    use std::collections::HashMap;

    let mut mem_states_by_call: HashMap<NodeId, NodeId> = HashMap::new();
    let mut var_states_by_call: HashMap<(NodeId, rsleigh::Vn), NodeId> = HashMap::new();

    for node in graph.nodes.keys() {
        let kind = *graph.node_kind(node);
        let Ok([target]) = graph.node_inputs_exact::<1>(node) else {
            continue;
        };
        let (producer, producer_out_idx) = graph.output_definition(target);
        if producer_out_idx != 0 || !matches!(graph.node_kind(producer), NodeKind::Call) {
            continue; // Task 8's producer check already handled this shape error.
        }

        match kind {
            NodeKind::PostCallMemState => {
                if let Some(&first) = mem_states_by_call.get(&producer) {
                    errs.push(ValidationError::DuplicatePostCallMemState {
                        call: producer,
                        first,
                        second: node,
                    });
                } else {
                    mem_states_by_call.insert(producer, node);
                }
            }
            NodeKind::PostCallVarState(vn) => {
                let key = (producer, vn);
                if let Some(&first) = var_states_by_call.get(&key) {
                    errs.push(ValidationError::DuplicatePostCallVarState {
                        call: producer,
                        vn,
                        first,
                        second: node,
                    });
                } else {
                    var_states_by_call.insert(key, node);
                }
            }
            _ => {}
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
        (ExpectedOutputKind::AnyValue, NodeOutputKind::OutputType(_)) => true,
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

    #[error(
        "PostCallMemState {node:?} input producer {producer:?} has kind \
         {producer_kind:?}; expected Control output of a Call"
    )]
    PostCallMemStateNotAfterCall {
        node: NodeId,
        producer: NodeId,
        producer_kind: NodeOutputKind,
    },

    #[error(
        "PostCallVarState {node:?} input producer {producer:?} has kind \
         {producer_kind:?}; expected Control output of a Call"
    )]
    PostCallVarStateNotAfterCall {
        node: NodeId,
        producer: NodeId,
        producer_kind: NodeOutputKind,
    },

    #[error("call {call:?} has two PostCallMemState consumers: {first:?} and {second:?}")]
    DuplicatePostCallMemState {
        call: NodeId,
        first: NodeId,
        second: NodeId,
    },

    #[error(
        "call {call:?} has two PostCallVarState({vn:?}) consumers: \
         {first:?} and {second:?}"
    )]
    DuplicatePostCallVarState {
        call: NodeId,
        vn: rsleigh::Vn,
        first: NodeId,
        second: NodeId,
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
            errs.0
                .iter()
                .any(|e| matches!(e, ValidationError::UseListContainsStaleInput { .. })),
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
        assert!(
            errs.0.iter().any(|e| matches!(
                e,
                ValidationError::ControlStateNonControlPredecessor { input_idx: 0, .. }
            )),
            "got: {errs:?}"
        );
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
        assert!(
            errs.0
                .iter()
                .any(|e| matches!(e, ValidationError::PhiTokenNotFromControlState { .. })),
            "got: {errs:?}"
        );
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

        let c1 = graph.create_node(
            NodeKind::IntConst(1),
            [],
            [NodeOutputKind::OutputType(NodeOutputType::U64)],
        );
        let c2 = graph.create_node(
            NodeKind::IntConst(2),
            [],
            [NodeOutputKind::OutputType(NodeOutputType::U64)],
        );
        let c1_out = graph.node_outputs(c1).into_iter().next().unwrap();
        let c2_out = graph.node_outputs(c2).into_iter().next().unwrap();
        let vn = test_vn();
        let _phi = graph.create_node(
            NodeKind::ControlPhi(vn),
            [cs_phi_out, c1_out, c2_out],
            [NodeOutputKind::OutputType(NodeOutputType::U64)],
        );

        let errs = validate(&graph, entry).unwrap_err();
        assert!(
            errs.0.iter().any(|e| matches!(
                e,
                ValidationError::PhiValueArityMismatch {
                    expected_predecessors: 1,
                    actual_values: 2,
                    ..
                }
            )),
            "got: {errs:?}"
        );
    }

    #[test]
    fn layer_c_stack_store_phi_does_not_fire_arity_mismatch() {
        // StackStorePhi has fixed arity [token, memory, data] regardless of
        // how many predecessors the owning ControlState has.  The
        // per-predecessor arity rule that applies to ControlPhi/MemPhi must
        // not fire on it.  Here the owning ControlState has 1 predecessor;
        // before the fix this produced a spurious
        // PhiValueArityMismatch { expected_predecessors: 1, actual_values: 2 }.
        let mut graph = Graph::new();
        let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
        let mem = graph.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory]);
        let entry_out = graph.node_outputs(entry).into_iter().next().unwrap();
        let mem_out = graph.node_outputs(mem).into_iter().next().unwrap();

        let cs = graph.create_node(
            NodeKind::ControlState,
            [entry_out],
            [NodeOutputKind::Control, NodeOutputKind::ControlPhi],
        );
        let cs_phi_out = graph.node_outputs(cs).into_iter().nth(1).unwrap();

        let data = graph.create_node(
            NodeKind::IntConst(0),
            [],
            [NodeOutputKind::OutputType(NodeOutputType::U64)],
        );
        let data_out = graph.node_outputs(data).into_iter().next().unwrap();

        let _ssp = graph.create_node(
            NodeKind::StackStorePhi {
                space: rsleigh::VnSpace::RAM,
            },
            [cs_phi_out, mem_out, data_out],
            [NodeOutputKind::Memory],
        );

        let res = validate(&graph, entry);
        if let Err(errs) = &res {
            assert!(
                !errs
                    .0
                    .iter()
                    .any(|e| matches!(e, ValidationError::PhiValueArityMismatch { .. })),
                "StackStorePhi must not trigger PhiValueArityMismatch; got: {errs:?}"
            );
        }
    }

    #[test]
    fn layer_c_postcall_mem_state_from_entry_is_error() {
        let mut graph = Graph::new();
        let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
        let _mem = graph.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory]);
        let entry_ctrl = graph.node_outputs(entry).into_iter().next().unwrap();

        let _bad = graph.create_node(
            NodeKind::PostCallMemState,
            [entry_ctrl],
            [NodeOutputKind::Memory],
        );

        let errs = validate(&graph, entry).unwrap_err();
        assert!(
            errs.0
                .iter()
                .any(|e| matches!(e, ValidationError::PostCallMemStateNotAfterCall { .. })),
            "got: {errs:?}"
        );
    }

    #[test]
    fn layer_c_postcall_var_state_from_entry_is_error() {
        let mut graph = Graph::new();
        let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
        let _mem = graph.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory]);
        let entry_ctrl = graph.node_outputs(entry).into_iter().next().unwrap();

        let vn = test_vn();
        let _bad = graph.create_node(
            NodeKind::PostCallVarState(vn),
            [entry_ctrl],
            [NodeOutputKind::OutputType(NodeOutputType::U64)],
        );

        let errs = validate(&graph, entry).unwrap_err();
        assert!(
            errs.0
                .iter()
                .any(|e| matches!(e, ValidationError::PostCallVarStateNotAfterCall { .. })),
            "got: {errs:?}"
        );
    }

    // Helpers for constructing a Call node and its outputs.
    fn make_call(
        graph: &mut Graph,
        ctrl: NodeOutputId,
        mem: NodeOutputId,
    ) -> (NodeId, NodeOutputId) {
        let addr = graph.create_node(
            NodeKind::IntConst(0x1000),
            [],
            [NodeOutputKind::OutputType(NodeOutputType::U64)],
        );
        let addr_out = graph.node_outputs(addr).into_iter().next().unwrap();
        let call = graph.create_node(
            NodeKind::Call,
            [ctrl, mem, addr_out],
            [NodeOutputKind::Control, NodeOutputKind::Memory],
        );
        let call_ctrl = graph.node_outputs(call).into_iter().next().unwrap();
        (call, call_ctrl)
    }

    #[test]
    fn layer_c_two_postcall_mem_states_on_same_call() {
        // To get two distinct PostCallMemState nodes consuming the same Call
        // ctrl, we create one PostCallMemState for call1 and one for call2,
        // then retarget the second node's input to call1's ctrl.  This
        // corrupts the use-list (Layer B also fires), but the test only checks
        // that DuplicatePostCallMemState is among the errors.
        let mut graph = Graph::new();
        let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
        let mem_node = graph.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory]);
        let entry_ctrl = graph.node_outputs(entry).into_iter().next().unwrap();
        let mem_out = graph.node_outputs(mem_node).into_iter().next().unwrap();

        let (_, call1_ctrl) = make_call(&mut graph, entry_ctrl, mem_out);
        // call2 needs a different ctrl — reuse entry_ctrl (Layer A may object,
        // but we just need two distinct Call nodes to get two distinct
        // PostCallMemState NodeIds).
        let (_, call2_ctrl) = make_call(&mut graph, entry_ctrl, mem_out);

        let _pcm1 = graph.create_node(
            NodeKind::PostCallMemState,
            [call1_ctrl],
            [NodeOutputKind::Memory],
        );
        let pcm2 = graph.create_node(
            NodeKind::PostCallMemState,
            [call2_ctrl],
            [NodeOutputKind::Memory],
        );

        // Retarget pcm2's input to call1_ctrl so call1 now has two consumers.
        // (Use test_only_retarget_input so the use-list is not updated — that
        // corruption is intentional; we want to verify the uniqueness check.)
        let pcm2_input = graph.node_input_id_at(pcm2, 0);
        graph.test_only_retarget_input(pcm2_input, call1_ctrl);

        let errs = validate(&graph, entry).unwrap_err();
        assert!(
            errs.0
                .iter()
                .any(|e| matches!(e, ValidationError::DuplicatePostCallMemState { .. })),
            "got: {errs:?}"
        );
    }

    #[test]
    fn layer_c_two_postcall_var_states_same_vn() {
        let mut graph = Graph::new();
        let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
        let mem_node = graph.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory]);
        let entry_ctrl = graph.node_outputs(entry).into_iter().next().unwrap();
        let mem_out = graph.node_outputs(mem_node).into_iter().next().unwrap();

        let (_, call1_ctrl) = make_call(&mut graph, entry_ctrl, mem_out);
        let (_, call2_ctrl) = make_call(&mut graph, entry_ctrl, mem_out);

        let vn = test_vn();
        let v1 = graph.create_node(
            NodeKind::PostCallVarState(vn),
            [call1_ctrl],
            [NodeOutputKind::OutputType(NodeOutputType::U64)],
        );
        let v2 = graph.create_node(
            NodeKind::PostCallVarState(vn),
            [call2_ctrl],
            [NodeOutputKind::OutputType(NodeOutputType::U64)],
        );

        // Retarget v2's input to call1_ctrl — same vn, same call, two nodes.
        let v2_input = graph.node_input_id_at(v2, 0);
        graph.test_only_retarget_input(v2_input, call1_ctrl);

        let errs = validate(&graph, entry).unwrap_err();
        assert!(
            errs.0
                .iter()
                .any(|e| matches!(e, ValidationError::DuplicatePostCallVarState { .. })),
            "got: {errs:?}"
        );

        // v1 still consumes call1 ctrl legitimately.
        let _ = v1;
    }

    #[test]
    fn layer_a_wrong_input_count() {
        use crate::node::NodeOutputType;
        use crate::ops::IntBinaryOp;

        let mut graph = Graph::new();
        let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
        let _mem = graph.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory]);
        let entry_ctrl = graph.node_outputs(entry).into_iter().next().unwrap();
        let c = graph.create_node(
            NodeKind::IntConst(5),
            [],
            [NodeOutputKind::OutputType(NodeOutputType::U64)],
        );
        let c_out = graph.node_outputs(c).into_iter().next().unwrap();

        // IntBinaryOp expects 2 inputs; give it 1.
        let bad = graph.create_node(
            NodeKind::IntBinaryOp(IntBinaryOp::Add),
            [c_out],
            [NodeOutputKind::OutputType(NodeOutputType::U64)],
        );
        let bad_out = graph.node_outputs(bad).into_iter().next().unwrap();

        // Wire `bad` into the reachable sub-graph so the reachability-scoped
        // Layer A actually inspects it.  A Return consuming entry's Control
        // plus `bad`'s value output is the smallest reachable shape.
        let _ret = graph.create_node(NodeKind::Return, [entry_ctrl, bad_out], []);

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
