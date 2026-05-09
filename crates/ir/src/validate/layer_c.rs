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
        // Round 9 Ask-8 R2 F2: gate the entire ControlState check on
        // reachability, not just the empty-input branch.  A non-reachable
        // ControlState zombie with stale non-Control inputs (left by some
        // future pass that surgery-edits without scrubbing) would
        // otherwise produce a false-positive
        // `ControlStateNonControlPredecessor` error and mask real
        // problems elsewhere.  The validator's stated tolerance for
        // detached zombies (see `validate`'s doc) requires this gate to
        // apply to both branches.
        if !reachable.contains(node) {
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
///
/// Reachability-scoped — every other Layer-C per-node check is
/// reachability-gated, and `RedundantPhis` may leave a stale
/// `FunctionArg` zombie in the arena while a new canonical one is live.
/// Without scoping, the validator would flag a structurally valid graph
/// with `DuplicateFunctionArg`.
pub(super) fn check_layer_c_function_arg_uniqueness(
    graph: &Graph,
    reachable: &NodeIdSet,
    errs: &mut Vec<ValidationError>,
) {
    use std::collections::HashMap;

    let mut by_index: HashMap<u32, NodeId> = HashMap::new();
    for node in graph.nodes.keys() {
        if !reachable.contains(node) {
            continue;
        }
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

/// Layer C: verify every reachable `IntConstWide(id)` node references
/// a live entry in `Graph::wide_consts` and that the stored value's
/// byte size matches the node's declared output type.
///
/// Emits [`ValidationError::DanglingWideConstId`] when the id is not
/// present in the side-table (caller bypassed `intern_wide_const`),
/// and [`ValidationError::WideConstWidthMismatch`] when the storage
/// width contradicts the output type (e.g. U256 storage with U512
/// declared output).
pub(super) fn check_layer_c_wide_consts(
    graph: &Graph,
    reachable: &NodeIdSet,
    errs: &mut Vec<ValidationError>,
) {
    use crate::node::NodeOutputType;
    for node in graph.nodes.keys() {
        if !reachable.contains(node) {
            continue;
        }
        let NodeKind::IntConstWide(id) = graph.node_kind(node) else {
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
        let Some(out) = outputs.into_iter().next() else {
            continue;
        };
        let NodeOutputKind::OutputType(ty) = graph.output_kind(out) else {
            continue;
        };
        let expected = match ty {
            NodeOutputType::U256 => 32,
            NodeOutputType::U512 => 64,
            _ => {
                // Output type isn't U256/U512 — this is a Layer-A signature
                // mismatch (IntConstWide's signature is `outputs: [INT_VAL]`,
                // any int passes Layer A but only U256/U512 are semantically
                // valid).  Surface as a width mismatch for the rare case
                // where a synthetic graph produces, say, IntConstWide on a
                // U64 output.
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
