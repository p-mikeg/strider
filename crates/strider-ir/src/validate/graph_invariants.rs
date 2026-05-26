use crate::function::Function;
use crate::graph::Graph;
use crate::node::{NodeId, NodeKind, NodeOutputId, NodeOutputKind};
use crate::walk::NodeIdSet;

use super::ValidationError;

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

/// Graph invariant: every input of a `Region` node must be a
/// `Control`-kinded output. Emits `RegionNonControlPredecessor`
/// per offending input.
pub(super) fn check_graph_invariants_region(
    graph: &Graph,
    reachable: &NodeIdSet,
    errs: &mut Vec<ValidationError>,
) {
    // Reachability is gated by `Graph::reachable_kind_iter` (see its
    // doc for why we skip detached `Region` zombies — they may
    // carry stale non-Control inputs left by an unscrubbed surgical
    // edit).
    for (node, kind) in graph.reachable_kind_iter(reachable) {
        if !matches!(kind, NodeKind::Region) {
            continue;
        }
        let inputs = graph.node_inputs(node);
        if inputs.is_empty() {
            errs.push(ValidationError::EmptyRegionPredecessors {
                region: node,
            });
            continue;
        }
        for (idx, target) in inputs.into_iter().enumerate() {
            let kind = graph.output_kind(target);
            if kind != NodeOutputKind::Control {
                let (producer, _) = graph.output_definition(target);
                errs.push(ValidationError::RegionNonControlPredecessor {
                    region: node,
                    input_idx: idx,
                    producer,
                    producer_kind: kind,
                });
            }
        }
    }
}

/// Graph invariant: every phi node (`Phi`, `MemPhi`) must take its
/// dispatch token (input[0]) from a `Region`'s `PhiToken` output.
///
/// For `Phi` and `MemPhi` (variadic phis), the number of value inputs
/// must match the owning `Region`'s predecessor count.
pub(super) fn check_graph_invariants_phis(
    graph: &Graph,
    reachable: &NodeIdSet,
    errs: &mut Vec<ValidationError>,
) {
    // Reachability is gated by `Graph::reachable_kind_iter`.
    // `RedundantPhis` and `DeadBranchElimination` leave zero-input phi
    // zombies in the arena; reaching one here would falsely trip
    // `PhiTokenNotFromRegion` (input[0] is gone).
    for (node, kind) in graph.reachable_kind_iter(reachable) {
        let is_phi = matches!(kind, NodeKind::Phi | NodeKind::MemPhi);
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
            errs.push(ValidationError::PhiTokenNotFromRegion {
                phi: node,
                producer,
                producer_kind: token_kind,
            });
            continue;
        }

        let (owner, _idx) = graph.output_definition(token);
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
    }
}

/// Graph invariant: every reachable `Call` node's output count and
/// every reachable `Return` node's input count match the function's
/// calling-convention metadata.
///
/// * `Call` outputs: `2 + clobber_count` where `clobber_count` is
///   the per-`Call` override length (when set) or the function-default
///   `call_clobbered_regs` length.  Slots 0/1 are Control / Memory(_);
///   slots 2.. are the clobber values.
/// * `Return` inputs: `2 + ret_val_count` where `ret_val_count` is
///   the function's combined int+float ret-val register count.
///
/// Catches the class of bugs where the orchestrator's
/// indirect-resolve in-place edits synthesise Returns / Calls whose
/// arity drifts from the function-default shape (e.g. silently
/// dropping `ret_val_regs_float`).
///
/// Skips the check when:
///
/// * `cc_metadata` is absent (synthetic graph that hasn't been built
///   through `FunctionBuilder::build`), OR
/// * the relevant CC list is empty — `Call` arity is unchecked when
///   `call_clobbered_regs` is empty AND no per-`Call` override is set;
///   `Return` arity is unchecked when `ret_val_regs` is empty.  This
///   is the synthetic-test escape hatch: `RegisterSet`-built fixtures
///   commonly track SP without declaring any ret-val regs and rely on
///   the variadic Return tail to ship an arbitrary value to assert
///   against.  Pinning arity against an empty CC list would block
///   that intentional usage without catching any real-world bug class
///   (the bug class — synthesised Return drops some declared
///   ret-val regs — only manifests when at least one ret-val reg IS
///   declared).
pub(super) fn check_graph_invariants_cc_arity(
    function: &Function,
    reachable: &NodeIdSet,
    errs: &mut Vec<ValidationError>,
) {
    let graph: &Graph = function;
    let ret_val_count = function.ret_val_regs().len();
    let default_clobber_count = function.call_clobbered_regs().len();
    // Synthetic / partially-built fixtures legitimately have empty CC
    // metadata recorded; skip arity checks since there's nothing to
    // arity-check against.
    if ret_val_count == 0 && default_clobber_count == 0 {
        return;
    }
    for (node, kind) in graph.reachable_kind_iter(reachable) {
        match kind {
            NodeKind::Call => {
                // Per-Call override length wins over the function
                // default; falls back to function-default when no
                // override was recorded.
                let override_count = function
                    .call_clobbered_override(node)
                    .map(<[_]>::len);
                let clobber_count = override_count.unwrap_or(default_clobber_count);
                // Synthetic-test escape: skip when both the function
                // default and the per-Call override are empty.
                if clobber_count == 0 {
                    continue;
                }
                let expected = 2 + clobber_count;
                let actual = graph.node_outputs(node).len();
                if actual != expected {
                    errs.push(ValidationError::NodeOutputCountMismatch {
                        node,
                        expected,
                        actual,
                    });
                }
            }
            NodeKind::Return => {
                if ret_val_count == 0 {
                    continue;
                }
                let expected = 2 + ret_val_count;
                let actual = graph.node_inputs(node).len();
                if actual != expected {
                    errs.push(ValidationError::NodeInputCountMismatch {
                        node,
                        expected,
                        actual,
                    });
                }
            }
            _ => {}
        }
    }
}

/// Graph invariant: every reachable, non-exempt node must carry at
/// least one asm-fingerprint contributor.  See
/// [`crate::function::Function::asm_fingerprint`] for the full contract.
pub(super) fn check_graph_invariants_asm_fingerprints(
    function: &Function,
    reachable: &NodeIdSet,
    errs: &mut Vec<ValidationError>,
) {
    let graph: &Graph = function;
    for (node, kind) in graph.reachable_kind_iter(reachable) {
        if kind.asm_fingerprint_exempt() {
            continue;
        }
        if function.asm_fingerprint(node).is_empty() {
            errs.push(ValidationError::MissingAsmFingerprint {
                node,
                kind: *kind,
            });
        }
    }
}

/// Returns the `(expected_byte_size, output_type)` pair that the
/// declared `NodeOutputType` of a wide-const node prescribes, or
/// `None` when the node lacks a value-typed output (skip — let
/// Layer A handle the structural error).
///
/// Emits a `WideConstWidthMismatch { expected_bytes: 0, actual_bytes
/// = actual }` sentinel when the declared output type isn't U256 or
/// U512: IntConstWide's local-typing signature accepts any `INT_VAL`
/// slot kind, but only U256/U512 are semantically valid storage
/// widths.  `expected_bytes = 0` flags the violation as "no valid
/// width" so callers can distinguish it from a genuine width
/// mismatch.
fn wide_const_expected_bytes(
    graph: &Graph,
    node: NodeId,
    actual: usize,
) -> Result<Option<(usize, crate::node::NodeOutputType)>, ValidationError> {
    use crate::node::NodeOutputType;
    let outputs = graph.node_outputs(node);
    let Some(&out) = outputs.first() else {
        return Ok(None);
    };
    let NodeOutputKind::OutputType(ty) = graph.output_kind(out) else {
        return Ok(None);
    };
    match ty {
        NodeOutputType::U256 => Ok(Some((32, ty))),
        NodeOutputType::U512 => Ok(Some((64, ty))),
        _ => Err(ValidationError::WideConstWidthMismatch {
            node,
            output_type: ty,
            expected_bytes: 0,
            actual_bytes: actual,
        }),
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
    for (node, kind) in graph.reachable_kind_iter(reachable) {
        let NodeKind::IntConstWide(id) = kind else {
            continue;
        };
        if graph.wide_consts.get(*id).is_none() {
            errs.push(ValidationError::DanglingWideConstId { node, id: *id });
            continue;
        }
        let actual = graph.wide_const(*id).byte_size();
        match wide_const_expected_bytes(graph, node, actual) {
            Err(e) => errs.push(e),
            Ok(Some((expected, ty))) if expected != actual => {
                errs.push(ValidationError::WideConstWidthMismatch {
                    node,
                    output_type: ty,
                    expected_bytes: expected,
                    actual_bytes: actual,
                });
            }
            Ok(_) => {}
        }
    }
}
