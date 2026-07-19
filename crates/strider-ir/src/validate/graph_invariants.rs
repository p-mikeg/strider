use crate::IRViewer;
use crate::function::Function;
use crate::graph::Graph;
use crate::node::{NodeId, NodeKind, ValueKind};
use crate::walk::NodeIdSet;

use super::ValidationError;

/// At most one live [`NodeKind::Entry`] and one live
/// [`NodeKind::InitialMemory`]. Neither is required.
pub(super) fn check_graph_invariants_uniqueness(
    graph: &Graph,
    reachable: &NodeIdSet,
    errs: &mut Vec<ValidationError>,
) {
    // Only the first two of each kind matter.
    let mut entry: (Option<NodeId>, Option<NodeId>) = (None, None);
    let mut initial_memory: (Option<NodeId>, Option<NodeId>) = (None, None);

    let record = |slot: &mut (Option<NodeId>, Option<NodeId>), node: NodeId| {
        if slot.0.is_none() {
            slot.0 = Some(node);
        } else if slot.1.is_none() {
            slot.1 = Some(node);
        }
    };

    for (node, kind) in reachable.iter().map(|n| (n, graph.node_kind(n))) {
        match kind {
            NodeKind::Entry => record(&mut entry, node),
            NodeKind::InitialMemory => record(&mut initial_memory, node),
            _ => {}
        }
    }

    if let (Some(first), Some(second)) = entry {
        errs.push(ValidationError::MultipleEntryNodes { first, second });
    }
    if let (Some(first), Some(second)) = initial_memory {
        errs.push(ValidationError::MultipleInitialMemoryNodes { first, second });
    }
}

/// Every reachable `Region` needs at least one predecessor.
pub(super) fn check_graph_invariants_region(
    graph: &Graph,
    reachable: &NodeIdSet,
    errs: &mut Vec<ValidationError>,
) {
    for (node, kind) in reachable.iter().map(|n| (n, graph.node_kind(n))) {
        if !matches!(kind, NodeKind::Region) {
            continue;
        }
        if graph.node_inputs(node).is_empty() {
            errs.push(ValidationError::EmptyRegionPredecessors { region: node });
        }
    }
}

/// Every reachable node's `Control` output must have exactly one consumer.
pub(super) fn check_graph_invariants_control_single_use(
    function: &Function,
    reachable: &NodeIdSet,
    errs: &mut Vec<ValidationError>,
) {
    let graph = function.graph();
    for (node, _kind) in function.reachable_kind_iter(reachable) {
        for &value in function.node_outputs(node).iter() {
            if function.value_kind(value) != ValueKind::Control {
                continue;
            }
            // Peek at most two uses to classify 0 / 1 / many.
            let mut uses = graph.value_uses(value);
            match (uses.next(), uses.next()) {
                (None, _) => errs.push(ValidationError::UnusedControlOutput { node, value }),
                (Some(_), Some(_)) => {
                    errs.push(ValidationError::ReusedControlOutput { node, value });
                }
                (Some(_), None) => {}
            }
        }
    }
}

/// `Extend` must strictly widen its input; `Truncate` must strictly narrow it.
/// Non-integer input/output is skipped.
pub(super) fn check_graph_invariants_extend_truncate(
    function: &Function,
    reachable: &NodeIdSet,
    errs: &mut Vec<ValidationError>,
) {
    let graph = function.graph();
    for (node, kind) in function.reachable_kind_iter(reachable) {
        let widening = match kind {
            NodeKind::Extend(_) => true,
            NodeKind::Truncate => false,
            _ => continue,
        };
        let (Some(in_value), Some(&out_value)) = (
            graph.node_inputs(node).into_iter().next(),
            graph.node_outputs(node).first(),
        ) else {
            continue;
        };
        let (Some(in_ty), Some(out_ty)) = (
            function.value_type_opt(in_value),
            function.value_type_opt(out_value),
        ) else {
            continue;
        };
        let (in_width, out_width) = (in_ty.bit_width(), out_ty.bit_width());
        let ok = if widening {
            out_width > in_width
        } else {
            out_width < in_width
        };
        if !ok {
            errs.push(ValidationError::ExtendTruncateWidthDirection {
                node,
                kind: *kind,
                in_width,
                out_width,
            });
        }
    }
}

/// Every reachable [`NodeKind::Switch`] needs at least one control output, and
/// one recorded `switch_targets` case address per control output.
pub(super) fn check_graph_invariants_switch(
    function: &Function,
    reachable: &NodeIdSet,
    errs: &mut Vec<ValidationError>,
) {
    let graph = function.graph();
    for (node, kind) in function.reachable_kind_iter(reachable) {
        if !matches!(kind, NodeKind::Switch) {
            continue;
        }
        let n_out = graph.node_outputs(node).len();
        let n_targets = function.side_tables().switch_targets(node).len();
        if n_out == 0 {
            errs.push(ValidationError::EmptySwitchTargets { node });
        } else if n_out != n_targets {
            errs.push(ValidationError::SwitchTargetArityMismatch {
                node,
                outputs: n_out,
                targets: n_targets,
            });
        }
    }
}

/// Every `Phi` / `MemPhi` takes its dispatch token (input[0]) from a
/// `Region`'s `PhiToken` output, and has one value input per predecessor of
/// that owning `Region`.
pub(super) fn check_graph_invariants_phis(
    function: &Function,
    reachable: &NodeIdSet,
    errs: &mut Vec<ValidationError>,
) {
    let graph = function.graph();
    for (node, kind) in function.reachable_kind_iter(reachable) {
        let is_phi = matches!(kind, NodeKind::Phi | NodeKind::MemPhi);
        if !is_phi {
            continue;
        }

        let inputs = graph.node_inputs(node);
        if inputs.is_empty() {
            // Local typing already reports empty-input phis.
            continue;
        }
        let token = inputs[0];
        let token_kind = graph.value_kind(token);
        if token_kind != ValueKind::PhiToken {
            let (producer, _) = graph.value_definition(token);
            errs.push(ValidationError::PhiTokenNotFromRegion {
                phi: node,
                producer,
                producer_kind: token_kind,
            });
            continue;
        }

        let (owner, _idx) = graph.value_definition(token);
        if !matches!(graph.node_kind(owner), NodeKind::Region) {
            errs.push(ValidationError::PhiTokenNotFromRegion {
                phi: node,
                producer: owner,
                producer_kind: token_kind,
            });
            continue;
        }

        let expected_preds = graph.node_inputs(owner).len();
        let actual_values = inputs.len() - 1;
        if expected_preds != actual_values {
            errs.push(ValidationError::PhiValueArityMismatch {
                phi: node,
                owner_region: owner,
                expected_predecessors: expected_preds,
                actual_values,
            });
        }

        // Every value input must carry the phi's own output type. `MemPhi` is
        // exempt: Memory tokens have no value type.
        if matches!(kind, NodeKind::Phi) {
            let phi_out_ty = function
                .first_value_output_of(node)
                .and_then(|o| function.value_type_opt(o));
            if let Some(out_ty) = phi_out_ty {
                for (i, inp) in inputs.iter().enumerate().skip(1) {
                    if let Some(in_ty) = graph.value_kind(inp).as_value()
                        && in_ty != out_ty
                    {
                        errs.push(ValidationError::PhiInputTypeMismatch {
                            phi: node,
                            input_index: i,
                            output_ty: out_ty,
                            input_ty: in_ty,
                        });
                    }
                }
            }
        }
    }
}

/// Every reachable, non-exempt node carries at least one asm-fingerprint
/// contributor.
pub(super) fn check_graph_invariants_asm_fingerprints(
    function: &Function,
    reachable: &NodeIdSet,
    errs: &mut Vec<ValidationError>,
) {
    for (node, kind) in function.reachable_kind_iter(reachable) {
        if kind.asm_fingerprint_exempt() {
            continue;
        }
        if function.side_tables().asm_fingerprint_is_empty(node) {
            errs.push(ValidationError::MissingAsmFingerprint { node, kind: *kind });
        }
    }
}

/// Every reachable `Store` has its Memory output consumed by a reachable node.
///
/// Scoped to `Store`: a memory-preserving `Call` / `CallOther` legitimately
/// leaves its Memory output unconsumed.
pub(super) fn check_graph_invariants_memory_chain(
    function: &Function,
    reachable: &NodeIdSet,
    errs: &mut Vec<ValidationError>,
) {
    let graph = function.graph();
    for (node, kind) in function.reachable_kind_iter(reachable) {
        if !matches!(kind, NodeKind::Store(_)) {
            continue;
        }
        for &out in graph.node_outputs(node) {
            if graph.value_kind(out) != ValueKind::Memory {
                continue;
            }
            let anchored = graph
                .value_uses(out)
                .any(|(consumer, _)| reachable.contains(consumer));
            if !anchored {
                errs.push(ValidationError::OrphanedMemoryOutput { node, kind: *kind });
            }
        }
    }
}

/// The side-indices must not have drifted from the live graph.
///
/// * Every reachable `initial_var_index` entry resolves to an
///   `InitialVar(vn)` node with the same varnode. Unreachable entries are
///   tolerated.
/// * Every `value_vn` key with a reachable producer is produced by a `Phi` /
///   `Call` / `CallOther`.
pub(super) fn check_graph_invariants_side_indices(
    function: &Function,
    reachable: &NodeIdSet,
    errs: &mut Vec<ValidationError>,
) {
    let graph = function.graph();
    for (vn, node) in function.initial_var_index_entries() {
        if !reachable.contains(node) {
            continue;
        }
        let kind = graph.node_kind(node);
        let matches =
            matches!(kind, NodeKind::InitialVar(found) if function.initial_vn(*found) == vn);
        if !matches {
            errs.push(ValidationError::StaleInitialVarIndex {
                node,
                vn,
                actual_kind: *kind,
            });
        }
    }

    for (value, vn) in function.value_vn_entries() {
        let producer = graph.producer(value);
        if !reachable.contains(producer) {
            continue;
        }
        let producer_kind = graph.node_kind(producer);
        if !matches!(
            producer_kind,
            NodeKind::Phi | NodeKind::Call | NodeKind::CallOther { .. }
        ) {
            errs.push(ValidationError::StaleValueVn {
                value,
                vn,
                producer,
                producer_kind: *producer_kind,
            });
        }
    }
}

/// Every reachable `IntConst(id)` references a live const-interner entry whose
/// value fits the node's declared output width.
pub(super) fn check_graph_invariants_consts(
    function: &crate::Function,
    reachable: &NodeIdSet,
    errs: &mut Vec<ValidationError>,
) {
    let graph = function.graph();
    for (node, kind) in function.reachable_kind_iter(reachable) {
        let NodeKind::IntConst(id) = *kind else {
            continue;
        };
        let Some(value) = function.const_interner.get(id) else {
            errs.push(ValidationError::DanglingConstId { node, id });
            continue;
        };
        let outputs = graph.node_outputs(node);
        let Some(&out) = outputs.first() else {
            continue; // arity reported elsewhere
        };
        let ValueKind::Typed(ty) = graph.value_kind(out) else {
            continue;
        };
        // Canonical masking: every bit above the declared width is zero.
        let too_wide = match value {
            crate::node::const_value::ConstValue::Bits(v) => v & !ty.bit_mask_u128() != 0,
            crate::node::const_value::ConstValue::Wide(limbs) => {
                limbs.len() * 64 > ty.bit_width()
                    && limbs
                        .iter()
                        .enumerate()
                        .any(|(i, &l)| (i + 1) * 64 > ty.bit_width() && l != 0)
            }
        };
        if too_wide {
            errs.push(ValidationError::ConstWidthMismatch { node, id });
        }
    }
}
