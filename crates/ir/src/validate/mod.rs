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
use crate::node::{NodeId, NodeInputId, NodeOutputId, NodeOutputKind, NodeOutputType};
use crate::node_signature::ExpectedOutputKind;
use crate::walk::{NodeIdSet, walk_graph};

mod layer_a;
mod layer_b;
mod layer_c;
#[cfg(test)]
mod tests;

use layer_a::check_layer_a;
use layer_b::check_layer_b;
use layer_c::{
    check_layer_c_control_state, check_layer_c_function_arg_uniqueness, check_layer_c_phis,
    check_layer_c_uniqueness,
};

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
///
/// # Errors
///
/// Returns a [`ValidationErrors`] bundle aggregating every Layer A / B / C
/// violation found in `graph`. Validation does not fail fast — every layer
/// runs to completion so the caller sees the full set of problems at once.
pub fn validate(graph: &Graph, entry: NodeId) -> Result<(), ValidationErrors> {
    // Drive the walk to completion and reuse its internal DenseEntitySet
    // tracker rather than re-collecting yielded NodeIds.  Saves N inserts
    // and one extra allocation per validate call.
    let mut walk = walk_graph(graph, entry);
    walk.by_ref().for_each(|_| {});
    let reachable: NodeIdSet = walk.visited;
    let mut errs: Vec<ValidationError> = Vec::new();

    for node in graph.nodes.keys() {
        if !reachable.contains(node) {
            continue;
        }
        check_layer_a(graph, node, &mut errs);
    }

    check_layer_b(graph, &mut errs);

    check_layer_c_uniqueness(graph, &mut errs);

    check_layer_c_control_state(graph, &reachable, &mut errs);

    check_layer_c_phis(graph, &mut errs);

    check_layer_c_function_arg_uniqueness(graph, &mut errs);

    if errs.is_empty() {
        Ok(())
    } else {
        Err(ValidationErrors(errs))
    }
}

/// Returns whether an actual [`NodeOutputKind`] satisfies the
/// [`ExpectedOutputKind`] declared by a [`NodeKind`]'s signature.
///
/// `AnyInt` matches any integer-typed output (U8, U16, U32, U64, U128, U256);
/// `AnyFloat` matches F32 or F64; `Bool` matches only `OutputType(Bool)`.
/// `Control`, `Memory`, and `PhiToken` match their identically-named
/// [`NodeOutputKind`] variants.
fn kind_matches(expected: ExpectedOutputKind, actual: NodeOutputKind) -> bool {
    match expected {
        ExpectedOutputKind::Control => matches!(actual, NodeOutputKind::Control),
        ExpectedOutputKind::Memory => matches!(actual, NodeOutputKind::Memory),
        ExpectedOutputKind::PhiToken => matches!(actual, NodeOutputKind::PhiToken),
        ExpectedOutputKind::Bool => {
            matches!(actual, NodeOutputKind::OutputType(NodeOutputType::Bool))
        }
        ExpectedOutputKind::AnyInt => {
            matches!(actual, NodeOutputKind::OutputType(t) if t.is_integer())
        }
        ExpectedOutputKind::AnyFloat => {
            matches!(actual, NodeOutputKind::OutputType(t) if t.is_float())
        }
        ExpectedOutputKind::AnyValue => matches!(actual, NodeOutputKind::OutputType(_)),
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

    #[error("ControlState {control_state:?} has zero predecessors")]
    EmptyControlStatePredecessors { control_state: NodeId },

    #[error(
        "phi node {phi:?} input[0] token producer {producer:?} has kind \
         {producer_kind:?}; expected PhiToken from a ControlState"
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

    #[error("duplicate FunctionArg at index {index}: {first:?} and {second:?}")]
    DuplicateFunctionArg {
        index: u32,
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
