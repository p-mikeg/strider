use crate::graph::Graph;
use crate::node::{NodeId, NodeKind, NodeOutputId, NodeOutputKind};
use crate::walk::NodeIdSet;

use super::ValidationError;

/// Layer C (shape check): enforce that the graph has exactly one
/// [`NodeKind::Entry`] node and exactly one [`NodeKind::InitialMemory`] node.
///
/// This intentionally scans every node in the arena (including detached
/// zombies left by unsound rewrites) so that an extra Entry/InitialMemory
/// is reported even when the second one isn't reachable from `entry`.
/// `MissingInitialMemoryNode` likewise fires if no InitialMemory exists at
/// all - the InitialMemory node is allocated by `FunctionBuilder::build_entry`
/// without a wire to Entry so a reachability-scoped check would miss it
/// for graphs that haven't yet linked memory to a consumer.
///
/// Emits [`ValidationError::MissingEntryNode`] /
/// [`ValidationError::MissingInitialMemoryNode`] when a kind is absent, and
/// [`ValidationError::MultipleEntryNodes`] /
/// [`ValidationError::MultipleInitialMemoryNodes`] (carrying the first two
/// offenders) when a kind appears more than once.
pub(super) fn check_layer_c_uniqueness(graph: &Graph, errs: &mut Vec<ValidationError>) {
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
pub(super) fn check_layer_c_control_state(
    graph: &Graph,
    reachable: &NodeIdSet,
    errs: &mut Vec<ValidationError>,
) {
    for node in graph.nodes.keys() {
        if !matches!(graph.node_kind(node), NodeKind::ControlState) {
            continue;
        }
        let inputs = graph.node_inputs(node);
        if inputs.is_empty() {
            // Zero-predecessor ControlStates are *expected* for zombies left
            // behind by `RedundantPhis::detach_unreachable_nodes`; they live
            // in the arena but are not reachable from the entry. The
            // invariant only applies to reachable ControlState nodes.
            if reachable.contains(node) {
                errs.push(ValidationError::EmptyControlStatePredecessors {
                    control_state: node,
                });
            }
            continue;
        }
        for (idx, target) in inputs.into_iter().enumerate() {
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

/// Layer C: every phi node (`VarPhi`, `MemPhi`, `StackStorePhi`) must take
/// its dispatch token (input[0]) from a `ControlState`'s `PhiToken` output.
///
/// For `VarPhi` and `MemPhi` (variadic phis), the number of value inputs
/// must match the owning `ControlState`'s predecessor count.  `StackStorePhi`
/// has fixed arity `[token, memory, data]` (Layer A enforces this) — its
/// per-predecessor information lives in the side-table
/// `Graph::stack_phi_offsets`, not in its inputs, so the per-predecessor
/// arity rule does not apply to it.
pub(super) fn check_layer_c_phis(
    graph: &Graph,
    reachable: &NodeIdSet,
    errs: &mut Vec<ValidationError>,
) {
    for node in graph.nodes.keys() {
        // Optimisation passes (`RedundantPhis`, `DeadBranchElimination`)
        // detach phi inputs and leave the zero-input zombie node in the
        // arena rather than physically removing it.  Reaching one here
        // would falsely trip `PhiTokenNotFromControlState` (input[0] is
        // gone).  Layer A is already reachability-scoped for the same
        // reason; mirror that.
        if !reachable.contains(node) {
            continue;
        }
        let is_phi = matches!(
            graph.node_kind(node),
            NodeKind::VarPhi(_)
                | NodeKind::MemPhi
                | NodeKind::StackStorePhi { .. }
                | NodeKind::ValuePhi
        );
        if !is_phi {
            continue;
        }

        let inputs: smallvec::SmallVec<[NodeOutputId; 4]> =
            graph.node_inputs(node).into_iter().collect();
        if inputs.is_empty() {
            continue; // Layer A fires a count or kind mismatch for empty-input phis; skip here.
        }
        let token = inputs[0];
        let token_kind = graph.output_kind(token);
        if token_kind != NodeOutputKind::PhiToken {
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

/// Returns `true` if `kind` is allowed to carry an empty asm-fingerprint
/// even when reachable from the entry.  Region / phi / initial-state
/// nodes are synthesised by the lifter without a contributing machine
/// instruction; their fingerprint legitimately stays empty.
fn asm_fingerprint_exempt(kind: &NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::Entry
            | NodeKind::InitialMemory
            | NodeKind::InitialVar(_)
            | NodeKind::FunctionArg { .. }
            | NodeKind::ControlState
            | NodeKind::MemPhi
            | NodeKind::VarPhi(_)
            | NodeKind::ValuePhi
            | NodeKind::StackStorePhi { .. }
    )
}

/// Layer C (opt-in): every reachable, non-exempt node must carry at
/// least one asm-fingerprint contributor.  See
/// [`crate::graph::Graph::asm_fingerprint`] for the full contract.
pub(super) fn check_layer_c_asm_fingerprints(
    graph: &Graph,
    reachable: &NodeIdSet,
    errs: &mut Vec<ValidationError>,
) {
    for node in graph.nodes.keys() {
        if !reachable.contains(node) {
            continue;
        }
        let kind = graph.node_kind(node);
        if asm_fingerprint_exempt(kind) {
            continue;
        }
        if graph.asm_fingerprint(node).is_empty() {
            errs.push(ValidationError::MissingAsmFingerprint {
                node,
                kind: *kind,
            });
        }
    }
}

/// Layer C: at most one [`NodeKind::FunctionArg`] per `index`.  The
/// [`opt::FunctionArgDetect`] pass emits one canonical node per argument
/// index; having two would mean patterns keyed by `matcher.function_arg(i)`
/// become ambiguous.
pub(super) fn check_layer_c_function_arg_uniqueness(
    graph: &Graph,
    errs: &mut Vec<ValidationError>,
) {
    use std::collections::HashMap;

    let mut by_index: HashMap<u32, NodeId> = HashMap::new();
    for node in graph.nodes.keys() {
        let index = match *graph.node_kind(node) {
            NodeKind::FunctionArg { index, .. } => index,
            _ => continue,
        };
        if let Some(&first) = by_index.get(&index) {
            errs.push(ValidationError::DuplicateFunctionArg {
                index,
                first,
                second: node,
            });
        } else {
            by_index.insert(index, node);
        }
    }
}
