use crate::graph::Graph;
use crate::node::{NodeId, NodeKind, NodeOutputId, NodeOutputKind};
use crate::walk::NodeIdSet;

use super::ValidationError;

/// Yields `(NodeId, &NodeKind)` for every node in the arena that is
/// reachable from entry, paired with its kind.  Used by every
/// per-node graph-invariants check that needs reachability scoping;
/// the uniqueness check intentionally bypasses this helper because it
/// also wants to flag detached zombies of `Entry`/`InitialMemory`.
fn reachable_nodes<'a>(
    graph: &'a Graph,
    reachable: &'a NodeIdSet,
) -> impl Iterator<Item = (NodeId, &'a NodeKind)> + 'a {
    graph
        .nodes
        .keys()
        .filter(move |&n| reachable.contains(n))
        .map(move |n| (n, graph.node_kind(n)))
}

/// Shape check: enforce that the graph has exactly one
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
pub(super) fn check_graph_invariants_uniqueness(graph: &Graph, errs: &mut Vec<ValidationError>) {
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

/// Graph invariant: every input of a `ControlState` node must be a
/// `Control`-kinded output. Emits `ControlStateNonControlPredecessor`
/// per offending input.
pub(super) fn check_graph_invariants_control_state(
    graph: &Graph,
    reachable: &NodeIdSet,
    errs: &mut Vec<ValidationError>,
) {
    // Reachability is gated by `reachable_nodes` (see its doc for why
    // we skip detached `ControlState` zombies — they may carry stale
    // non-Control inputs left by an unscrubbed surgical edit).
    for (node, kind) in reachable_nodes(graph, reachable) {
        if !matches!(kind, NodeKind::ControlState) {
            continue;
        }
        let inputs = graph.node_inputs(node);
        if inputs.is_empty() {
            errs.push(ValidationError::EmptyControlStatePredecessors {
                control_state: node,
            });
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

/// Graph invariant: every phi node (`Phi`, `MemPhi`, `StackStorePhi`)
/// must take its dispatch token (input[0]) from a `ControlState`'s
/// `PhiToken` output.
///
/// For `Phi` and `MemPhi` (variadic phis), the number of value inputs
/// must match the owning `ControlState`'s predecessor count.  `StackStorePhi`
/// has fixed arity `[token, memory, data]` (local-typing enforces this) — its
/// per-predecessor information lives in the side-table
/// `Graph::stack_phi_offsets`, not in its inputs, so the per-predecessor
/// arity rule does not apply to it.
pub(super) fn check_graph_invariants_phis(
    graph: &Graph,
    reachable: &NodeIdSet,
    errs: &mut Vec<ValidationError>,
) {
    // Reachability is gated by `reachable_nodes`. `RedundantPhis` and
    // `DeadBranchElimination` leave zero-input phi zombies in the
    // arena; reaching one here would falsely trip
    // `PhiTokenNotFromControlState` (input[0] is gone).
    for (node, kind) in reachable_nodes(graph, reachable) {
        let is_phi = matches!(
            kind,
            NodeKind::Phi(_) | NodeKind::MemPhi | NodeKind::StackStorePhi { .. }
        );
        if !is_phi {
            continue;
        }

        let inputs: smallvec::SmallVec<[NodeOutputId; 4]> =
            graph.node_inputs(node).into_iter().collect();
        if inputs.is_empty() {
            // The local-typing check fires a count or kind mismatch for
            // empty-input phis; skip here.
            continue;
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
        if matches!(kind, NodeKind::StackStorePhi { .. }) {
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
            | NodeKind::Phi(_)
            | NodeKind::StackStorePhi { .. }
    )
}

/// Graph invariant: every reachable, non-exempt node must carry at
/// least one asm-fingerprint contributor.  See
/// [`crate::graph::Graph::asm_fingerprint`] for the full contract.
pub(super) fn check_graph_invariants_asm_fingerprints(
    graph: &Graph,
    reachable: &NodeIdSet,
    errs: &mut Vec<ValidationError>,
) {
    for (node, kind) in reachable_nodes(graph, reachable) {
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

/// Graph invariant: at most one [`NodeKind::FunctionArg`] per `index`.
/// The [`opt::FunctionArgDetect`] pass emits one canonical node per
/// argument index; having two would mean patterns keyed by
/// `matcher.function_arg(i)` become ambiguous.
///
/// Reachability-scoped — every other per-node graph-invariants check
/// is reachability-gated, and `RedundantPhis` may leave a stale
/// `FunctionArg` zombie in the arena while a new canonical one is live.
/// Without scoping, the validator would flag a structurally valid graph
/// with `DuplicateFunctionArg`.
pub(super) fn check_graph_invariants_function_arg_uniqueness(
    graph: &Graph,
    reachable: &NodeIdSet,
    errs: &mut Vec<ValidationError>,
) {
    use std::collections::HashMap;

    let mut by_index: HashMap<u32, NodeId> = HashMap::new();
    for (node, kind) in reachable_nodes(graph, reachable) {
        let index = match *kind {
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

/// Graph invariant: verify every reachable `IntConstWide(id)` node
/// references a live entry in `Graph::wide_consts` and that the stored
/// value's byte size matches the node's declared output type.
///
/// Emits [`ValidationError::DanglingWideConstId`] when the id is not
/// present in the side-table (caller bypassed `intern_wide_const`),
/// and [`ValidationError::WideConstWidthMismatch`] when the storage
/// width contradicts the output type (e.g. U256 storage with U512
/// declared output).
pub(super) fn check_graph_invariants_wide_consts(
    graph: &Graph,
    reachable: &NodeIdSet,
    errs: &mut Vec<ValidationError>,
) {
    use crate::node::NodeOutputType;
    for (node, kind) in reachable_nodes(graph, reachable) {
        let NodeKind::IntConstWide(id) = kind else {
            continue;
        };
        if graph.wide_consts.get(*id).is_none() {
            errs.push(ValidationError::DanglingWideConstId {
                node,
                id: *id,
            });
            continue;
        }
        let actual = graph.wide_const(*id).byte_size();
        let outputs = graph.node_outputs(node);
        let Some(&out) = outputs.first() else {
            continue;
        };
        let NodeOutputKind::OutputType(ty) = graph.output_kind(out) else {
            continue;
        };
        let expected = match ty {
            NodeOutputType::U256 => 32,
            NodeOutputType::U512 => 64,
            _ => {
                // Output type isn't U256/U512 — this is a local-typing
                // signature mismatch (IntConstWide's signature is
                // `outputs: [INT_VAL]`, any int passes local-typing but only
                // U256/U512 are semantically valid).  Surface as a width
                // mismatch for the rare case where a synthetic graph
                // produces, say, IntConstWide on a U64 output.
                errs.push(ValidationError::WideConstWidthMismatch {
                    node,
                    output_type: ty,
                    expected_bytes: 0,
                    actual_bytes: actual,
                });
                continue;
            }
        };
        if expected != actual {
            errs.push(ValidationError::WideConstWidthMismatch {
                node,
                output_type: ty,
                expected_bytes: expected,
                actual_bytes: actual,
            });
        }
    }
}
