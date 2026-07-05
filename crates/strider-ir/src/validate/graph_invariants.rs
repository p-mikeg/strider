use crate::IRViewer;
use crate::function::Function;
use crate::graph::Graph;
use crate::node::{NodeId, NodeKind, ValueKind};
use crate::walk::NodeIdSet;

use super::ValidationError;

/// Shape check: at most one LIVE [`NodeKind::Entry`] and at most one LIVE
/// [`NodeKind::InitialMemory`] node.
///
/// Scoped to the entry-reachable set like every other graph-invariant check —
/// a detached/zombie root left by a rewrite (or a stale node a `compact` bug
/// failed to drop) is unreachable garbage and is ignored. Neither root is
/// required to be present: `Entry` is the walk root so it always is, and
/// `InitialMemory` is OPTIONAL — a function performing no memory operations has
/// no reachable `InitialMemory`, which is valid (the eagerly-built one is just
/// unreachable and culled by `compact`). Only a *duplicate* live root is a
/// malformation.
///
/// Emits [`ValidationError::MultipleEntryNodes`] /
/// [`ValidationError::MultipleInitialMemoryNodes`] (carrying the first two
/// offenders) when a kind appears more than once in the live graph.
pub(super) fn check_graph_invariants_uniqueness(
    graph: &Graph,
    reachable: &NodeIdSet,
    errs: &mut Vec<ValidationError>,
) {
    // Only the first two of each kind matter (single vs the first duplicate
    // pair), so buffer two `Option` slots per kind instead of two heap `Vec`s.
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

/// Graph invariant: every reachable `Region` node must have at least one
/// predecessor. Emits `EmptyRegionPredecessors`. (The "every predecessor is
/// Control" rule is left to `check_local_typing` against the Region
/// signature's variadic CTRL tail.)
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
        // A Region with zero predecessors is malformed.  (The per-input
        // "must be Control" rule is NOT checked here — it is already enforced
        // by `check_local_typing` against the Region signature's variadic CTRL
        // tail, reported as a `NodeInputKindMismatch`.  Empty-arity, on the
        // other hand, local typing cannot express: the variadic tail permits
        // zero inputs, so this check is the only thing pinning >= 1.)
        if graph.node_inputs(node).is_empty() {
            errs.push(ValidationError::EmptyRegionPredecessors { region: node });
        }
    }
}

/// Graph invariant: every reachable node's `Control` output must be consumed by
/// exactly one node — a control edge has exactly one successor. Zero consumers
/// is a dangling control path (`UnusedControlOutput`) — every control edge must
/// reach a terminator (`Return` / `IndirectBranch` / `Unreachable`); two or more
/// is a malformed fan-out (`ReusedControlOutput`) that must instead be produced
/// by an `If` (split) or go through a `Region` (merge). Ported from spidir's
/// `verify_control_outputs`. No-return traps reach a terminator because the
/// lifter sinks their control into an `Unreachable`.
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
            // O(1): peek at most the first two uses to classify 0 / 1 / many.
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

/// Graph invariant: `Extend` must strictly widen its input and `Truncate`
/// must strictly narrow it.
///
/// The two ops are direction-typed by name: `Extend` fills *new* high bits
/// (zero or sign) and so only makes sense when the output is wider;
/// `Truncate` drops high bits and only makes sense when the output is
/// narrower. The low bits of a hypothetical non-widening `Extend` are
/// identical to a `Truncate` (and vice versa), so allowing the wrong
/// direction would give one value two legal node shapes — the redundant
/// spelling the canonical IR exists to avoid. The builder never mints these
/// (`extend_if_needed` / `truncate_to` dispatch on direction), so any that
/// appear are a malformed surgical edit.
///
/// Skips a node whose single input/output isn't integer-typed — the
/// local-typing check already reports that shape error, and reading a width
/// off a non-`Typed` value is meaningless.
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
        // Single INT input, single INT output. Bail to the local-typing check
        // if the arity/kind is off (missing slot or non-integer type).
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

/// Graph invariant: every reachable [`NodeKind::Switch`] must have at least
/// one control output, and its control-output count must equal its recorded
/// case-address count in `Function::switch_targets` — one output per case
/// address, kept in sync by `FunctionBuilder::build_switch`. A mismatch means
/// the side table has drifted from the graph shape (e.g. a surgical edit
/// added/removed an output without updating `switch_targets`).
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

/// Graph invariant: every phi node (`Phi`, `MemPhi`) must take its
/// dispatch token (input[0]) from a `Region`'s `PhiToken` output.
///
/// For `Phi` and `MemPhi` (variadic phis), the number of value inputs
/// must match the owning `Region`'s predecessor count.
pub(super) fn check_graph_invariants_phis(
    function: &Function,
    reachable: &NodeIdSet,
    errs: &mut Vec<ValidationError>,
) {
    let graph = function.graph();
    // Iterate the reachable set directly (ascending NodeId order).
    // `PhiCollapse` and `DeadBranchElimination` leave zero-input phi
    // zombies in the arena; reaching one here would falsely trip
    // `PhiTokenNotFromRegion` (input[0] is gone).
    for (node, kind) in function.reachable_kind_iter(reachable) {
        let is_phi = matches!(kind, NodeKind::Phi | NodeKind::MemPhi);
        if !is_phi {
            continue;
        }

        let inputs = graph.node_inputs(node);
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
        if function.side_tables().asm_fingerprint_is_empty(node) {
            errs.push(ValidationError::MissingAsmFingerprint { node, kind: *kind });
        }
    }
}

/// Graph invariant: every reachable `Store` must keep its (sole) Memory
/// output consumed by at least one reachable node, so the store stays
/// anchored in the live memory chain back to a `Return` / `IndirectBranch`
/// terminator (both consume memory).
///
/// A `Store` outputs `[MEM]` only (no Control output — see `node_signature`),
/// so `Function::compact` / `retain_reachable` keeps it solely because its
/// memory output is consumed by the next chain node.  If a future memory pass
/// repoints that consumer away (a memory-output `replace_value`, a
/// dead-store-elimination edit) while keeping the store reachable by some
/// other edge, the store is orphaned from the chain yet still emitted; this
/// check turns "a reachable store is anchored" from an emergent property into
/// an enforced one.
///
/// Scope is deliberately `Store` only, NOT every Memory-output producer:
/// a memory-PRESERVING `Call` / `CallOther` (`preserves_memory` /
/// `clobbers_memory == false`) legitimately leaves its Memory output
/// unconsumed — the builder emits the output unconditionally but advances the
/// region's memory through it only when the call clobbers memory ("you don't
/// have to use it", see `build_call_kind`).  Flagging those would reject the
/// canonical lifted shape of memory-preserving intrinsics (e.g. MIPS
/// division, lifted as a `CallOther`).  `MemPhi` / `InitialMemory` are
/// likewise excluded: their single Memory output makes "reachable" already
/// imply "consumed", so a check would be vacuous.  `Load` is never a producer
/// of a Memory edge (it outputs `[INT_VAL]`).
///
/// A consumer in DEAD control does not count: it is itself removable, so a
/// memory output reaching only unreachable nodes is not truly anchored.
///
/// Emits [`ValidationError::OrphanedMemoryOutput`] per offending node.
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
            // A memory output anchored only by an unreachable consumer is not
            // in the live chain; require a reachable use.
            let anchored = graph
                .value_uses(out)
                .any(|(consumer, _)| reachable.contains(consumer));
            if !anchored {
                errs.push(ValidationError::OrphanedMemoryOutput { node, kind: *kind });
            }
        }
    }
}

/// Graph invariant: the advisory side-indices must not have drifted from the
/// live graph.
///
/// * Every `initial_var_index` entry whose node is REACHABLE must resolve to an
///   `InitialVar(vn)` node with the SAME varnode.  A culled-but-not-yet-
///   compacted entry (node not reachable) is tolerated — that is the documented
///   mid-pipeline state `initial_sp` tolerates (it does not filter liveness) — but a
///   reachable node whose payload was rewritten away from `InitialVar(vn)` is a
///   genuine desync (the NodeId survived, so `compact` won't drop the entry).
/// * Every `value_vn` key whose PRODUCER is reachable must be produced by a
///   `Phi` / `Call` / `CallOther` (the only populations that carry a tag).
///
/// Emits [`ValidationError::StaleInitialVarIndex`] /
/// [`ValidationError::StaleValueVn`] per offending entry.
pub(super) fn check_graph_invariants_side_indices(
    function: &Function,
    reachable: &NodeIdSet,
    errs: &mut Vec<ValidationError>,
) {
    let graph = function.graph();
    for (vn, node) in function.initial_var_index_entries() {
        // A culled (unreachable) zombie entry is tolerated mid-pipeline.
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
        // Skip tags on values whose producer is no longer reachable.
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

/// Graph invariant: verify every reachable `IntConst(id)` node references a
/// live entry in `Function::const_interner` and that the interned value does
/// not exceed the node's declared output width.
///
/// Emits [`ValidationError::DanglingConstId`] when the id is not present in
/// the interner (caller bypassed `intern_int_const*`), and
/// [`ValidationError::ConstWidthMismatch`] when the stored value has bits set
/// above the declared type's bit width (non-canonical masking).
pub(super) fn check_graph_invariants_consts(
    function: &crate::Function,
    reachable: &NodeIdSet,
    errs: &mut Vec<ValidationError>,
) {
    use crate::node::NodeKind;
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
        // Every bit above the declared width must be zero (canonical masking).
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
