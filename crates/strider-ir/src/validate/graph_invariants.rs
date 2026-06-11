use crate::function::Function;
use crate::graph::Graph;
use crate::node::{NodeId, NodeKind, ValueId, ValueKind};
use crate::walk::NodeIdSet;
use crate::IRViewer;

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

    for node in graph.all_node_ids() {
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
    // Iterate the reachable set directly (ascending NodeId order; see the
    // note on why we skip detached `Region` zombies — they may carry stale
    // non-Control inputs left by an unscrubbed surgical edit).
    for (node, kind) in reachable.iter().map(|n| (n, graph.node_kind(n))) {
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
            let kind = graph.value_kind(target);
            if kind != ValueKind::Control {
                let (producer, _) = graph.value_definition(target);
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
    // Iterate the reachable set directly (ascending NodeId order).
    // `PhiCollapse` and `DeadBranchElimination` leave zero-input phi
    // zombies in the arena; reaching one here would falsely trip
    // `PhiTokenNotFromRegion` (input[0] is gone).
    for (node, kind) in reachable.iter().map(|n| (n, graph.node_kind(n))) {
        let is_phi = matches!(kind, NodeKind::Phi | NodeKind::MemPhi);
        if !is_phi {
            continue;
        }

        let inputs: smallvec::SmallVec<[ValueId; 4]> =
            graph.node_inputs(node).into_iter().collect();
        if inputs.is_empty() {
            // The local-typing check fires a count or kind mismatch for
            // empty-input phis; skip here.
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

        // Value-phi type consistency: every value input must carry the same
        // value type as the phi's own output.  (`MemPhi` merges Memory
        // tokens, which have no value type, so it is exempt; a non-value
        // input is already reported by the local-typing check, so only
        // mismatched concrete value types are flagged here.)
        if matches!(kind, NodeKind::Phi) {
            let phi_out_ty = graph
                .node_outputs(node)
                .iter()
                .find_map(|&o| graph.value_kind(o).as_value());
            if let Some(out_ty) = phi_out_ty {
                for (i, &inp) in inputs.iter().enumerate().skip(1) {
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

/// Graph invariant: every reachable `Call` node's output count and
/// every reachable `Return` node's input count match the function's
/// calling-convention metadata.
///
/// * `Call` outputs: `2 + ret_val_count + clobber_count`.  Slots 0/1
///   are Control / Memory(_); slots 2..2+ret_val_count are the return
///   values; slots 2+ret_val_count.. are the clobber values.
///   `ret_val_count` is the function-default combined int+float ret-val
///   register count; `clobber_count` is the per-`Call` override length
///   (when set) or the function-default `call_clobbered_regs` length.
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
/// * the calling-convention lists are empty (synthetic graph that hasn't been built
///   through `FunctionBuilder::build`), OR
/// * the relevant CC list is empty — a function-default `Call`'s arity
///   is unchecked when `call_clobbered_regs` is empty (an override
///   `Call`, identified by a recorded `call_cc`, is instead checked
///   against its tagged clobber outputs);
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
    let ret_val_count = function.ret_val_regs().len();
    let default_ret_val_count = function.call_ret_val_regs().len();
    let default_clobber_count = function.call_clobbered_regs().len();
    // No top-level early-return on empty defaults: an override Call
    // (recorded via `call_cc`) is checked against its tagged output
    // values even when the function defaults are empty.  The per-node
    // escapes below preserve the synthetic / partially-built-fixture
    // behaviour node by node.
    for (node, kind) in function.reachable_kind_iter(reachable) {
        match kind {
            NodeKind::Call => {
                let outputs = function.node_outputs(node);
                let actual = outputs.len();
                if let Some(cc) = function.call_cc(node) {
                    // Override Call: cross-check arity against the override CC's
                    // ret-val + clobber lists (projected onto the function's
                    // tracked set) — the SAME derivation the Call was built
                    // from.  Deriving from the node's own `value_vn` tags would
                    // be tautological: dropping a clobber output AND its tag
                    // changes the tag count and the actual count in lockstep, so
                    // a wrong-arity Call would pass silently.
                    let expected = 2
                        + function.call_ret_vals_for(cc).len()
                        + function.call_clobbered_for(cc).len();
                    if actual != expected {
                        errs.push(ValidationError::NodeOutputCountMismatch {
                            node,
                            expected,
                            actual,
                        });
                    }
                } else {
                    // Function-default Call: arity against the function's
                    // default ret-val + clobber lists.  Synthetic-test escape:
                    // skip when both defaults are empty (trivial CC).
                    if default_ret_val_count == 0 && default_clobber_count == 0 {
                        continue;
                    }
                    let expected = 2 + default_ret_val_count + default_clobber_count;
                    if actual != expected {
                        errs.push(ValidationError::NodeOutputCountMismatch {
                            node,
                            expected,
                            actual,
                        });
                    }
                }
            }
            NodeKind::Return => {
                if ret_val_count == 0 {
                    continue;
                }
                let expected = 2 + ret_val_count;
                let actual = function.node_inputs(node).len();
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
    for (node, kind) in function.reachable_kind_iter(reachable) {
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
/// declared `ValueType` of a wide-const node prescribes, or
/// `None` when the node lacks a value-typed output (skip — let
/// Layer A handle the structural error).
///
/// Emits [`ValidationError::WideConstInvalidOutputType`] when the declared
/// output type isn't one of the valid wide-const storage types (I80, I128,
/// I256, I512): `IntConst(Wide)`'s local-typing signature accepts any
/// `INT_VAL` slot kind, but only these four are semantically valid.
fn wide_const_expected_bytes(
    graph: &Graph,
    node: NodeId,
) -> Result<Option<(usize, crate::node::ValueType)>, ValidationError> {
    let outputs = graph.node_outputs(node);
    let Some(&out) = outputs.first() else {
        return Ok(None);
    };
    let ValueKind::Typed(ty) = graph.value_kind(out) else {
        return Ok(None);
    };
    if ty.is_wide_int() {
        Ok(Some((ty.byte_size(), ty)))
    } else {
        Err(ValidationError::WideConstInvalidOutputType {
            node,
            output_type: ty,
        })
    }
}

/// Graph invariant: verify every reachable `IntConst(Wide(id))` node
/// references a live entry in `Function::wide_const_interner` and that the
/// stored value's byte size matches the node's declared output type.
///
/// Emits [`ValidationError::DanglingWideConstId`] when the id is not
/// present in the side-table (caller bypassed `intern_wide_const`),
/// and [`ValidationError::WideConstWidthMismatch`] when the storage
/// width contradicts the output type (e.g. I256 storage with I512
/// declared output).
pub(super) fn check_graph_invariants_wide_consts(
    function: &crate::Function,
    reachable: &NodeIdSet,
    errs: &mut Vec<ValidationError>,
) {
    use crate::node::IntPayload;
    let graph = function.graph();
    for (node, kind) in function.reachable_kind_iter(reachable) {
        let NodeKind::IntConst(IntPayload::Wide(id)) = kind else {
            continue;
        };
        let Some(storage) = function.wide_const_opt(*id) else {
            errs.push(ValidationError::DanglingWideConstId { node, id: *id });
            continue;
        };
        let actual = storage.byte_size();
        match wide_const_expected_bytes(graph, node) {
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
