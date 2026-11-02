use cranelift_entity::SecondaryMap;

use crate::DominatorTree;
use crate::IRViewer;
use crate::function::Function;
use crate::graph::Graph;
use crate::node::{
    Conversion, NodeId, NodeKind, OperandWidth, ValueId, ValueKind, ValueType, WidthTie, width_tie,
};
use crate::schedule::{ScheduleContext, data_producers, schedule_early};
use crate::walk::{NodeIdSet, PostOrder, WalkPhase};

use super::ValidationError;

/// The data producers of a node, restricted to `universe`. `Phi` / `MemPhi`
/// expand to nothing: closing a data cycle is what they are for.
#[derive(Clone, Copy)]
struct DataProducers<'a> {
    graph: &'a Graph,
    universe: &'a NodeIdSet,
}

impl DataProducers<'_> {
    fn of(self, node: NodeId) -> impl Iterator<Item = NodeId> {
        let phi = matches!(self.graph.node_kind(node), NodeKind::Phi | NodeKind::MemPhi);
        (!phi)
            .then(|| data_producers(self.graph, node))
            .into_iter()
            .flatten()
            .filter(move |&producer| self.universe.contains(producer))
    }
}

impl graph_algorithms::walk::GraphRef for DataProducers<'_> {
    type NodeId = NodeId;

    fn successors(&self, node: NodeId, mut f: impl FnMut(NodeId)) {
        for succ in self.of(node) {
            f(succ);
        }
    }
}

/// No non-phi node is its own transitive data producer.
///
/// The DFS post-order's Pre/Post events bound the current path, so a producer
/// still on it is a back edge.
pub(super) fn check_function_invariants_data_cycles(
    function: &Function,
    reachable: &NodeIdSet,
    errs: &mut Vec<ValidationError>,
) {
    let producers = DataProducers {
        graph: function.graph(),
        universe: reachable,
    };
    let mut walk = PostOrder::new(producers, reachable.iter());
    // Not a `NodeIdSet`: the walk is seeded from every reachable node, so the
    // path empties once per seed, and `DenseEntitySet::remove` costs
    // O(max_index / 64) each time it drops the set's maximum.
    let mut on_path: SecondaryMap<NodeId, bool> = SecondaryMap::new();
    while let Some((phase, node)) = walk.next_event() {
        match phase {
            WalkPhase::Pre => {
                on_path[node] = true;
                if producers.of(node).any(|p| on_path[p]) {
                    errs.push(ValidationError::DataCycle {
                        node,
                        kind: *function.node_kind(node),
                    });
                }
            }
            WalkPhase::Post => on_path[node] = false,
        }
    }
}

/// The first two live [`NodeKind::Entry`] and [`NodeKind::InitialMemory`]
/// nodes; only the first two of each decide the verdict.
#[derive(Default)]
pub(super) struct UniqueKinds {
    entry: (Option<NodeId>, Option<NodeId>),
    initial_memory: (Option<NodeId>, Option<NodeId>),
}

pub(super) fn note_unique_kinds(seen: &mut UniqueKinds, node: NodeId, kind: &NodeKind) {
    let slot = if *kind == NodeKind::Entry {
        &mut seen.entry
    } else if *kind == NodeKind::InitialMemory {
        &mut seen.initial_memory
    } else {
        return;
    };
    if slot.0.is_none() {
        slot.0 = Some(node);
    } else if slot.1.is_none() {
        slot.1 = Some(node);
    }
}

/// At most one live [`NodeKind::Entry`] and one live
/// [`NodeKind::InitialMemory`]. Neither is required.
pub(super) fn check_unique_kinds(seen: &UniqueKinds, errs: &mut Vec<ValidationError>) {
    if let (Some(first), Some(second)) = seen.entry {
        errs.push(ValidationError::MultipleEntryNodes { first, second });
    }
    if let (Some(first), Some(second)) = seen.initial_memory {
        errs.push(ValidationError::MultipleInitialMemoryNodes { first, second });
    }
}

/// The type of `node`'s first output; `None` without one or when untyped.
fn first_output_type(graph: &Graph, node: NodeId) -> Option<ValueType> {
    graph
        .node_outputs(node)
        .first()
        .and_then(|&o| graph.value_kind(o).as_value())
}

/// Every reachable `Region` needs at least one predecessor.
pub(super) fn check_region(
    function: &Function,
    node: NodeId,
    kind: &NodeKind,
    errs: &mut Vec<ValidationError>,
) {
    let graph = function.graph();
    if !matches!(kind, NodeKind::Region) {
        return;
    }
    if graph.node_inputs(node).is_empty() {
        errs.push(ValidationError::EmptyRegionPredecessors { region: node });
    }
}

/// Every reachable node's `Control` output must have exactly one consumer.
pub(super) fn check_control_single_use(
    function: &Function,
    node: NodeId,
    errs: &mut Vec<ValidationError>,
) {
    let graph = function.graph();
    for &value in function.node_outputs(node) {
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

/// An arithmetic node is evaluated at ONE width, which every operand
/// [`NodeKind::operand_width`] ties to it must carry: the output's, or a
/// comparison's first operand's (its output is `I1`). A shift's count is
/// free, as in p-code. A bitcast keeps its byte size, which for a pair its
/// signature admits (one integer side, one float side) is the same claim: no
/// float is one byte, so only `I1`'s byte size could disagree with its bit
/// width and no float can pair with it. On a graph that already breaks the
/// slot-kind check the two differ, and the extra mismatch joins the kind
/// error rather than replacing it.
/// `Extend` / `Truncate` carry a DIRECTION instead and are checked by
/// [`check_extend_truncate`].
pub(super) fn check_arith_widths(
    function: &Function,
    node: NodeId,
    kind: &NodeKind,
    errs: &mut Vec<ValidationError>,
) {
    let graph = function.graph();
    let out = first_output_type(graph, node);
    // Carries the real input slot, not a position in the filtered list: a
    // node with an untyped input would otherwise report an index that
    // names a different operand, and read the wrong slot's width rule.
    let operands: Vec<(usize, ValueType)> = graph
        .node_inputs(node)
        .into_iter()
        .enumerate()
        .filter_map(|(slot, v)| function.value_type_opt(v).map(|t| (slot, t)))
        .collect();
    let first = operands.first().map(|&(_, t)| t);
    for &(slot, got) in &operands {
        let want = match width_tie(kind, slot, out, first) {
            WidthTie::Tied(want) => want,
            WidthTie::Conversion(Conversion::SameSize) => match out {
                Some(out) => out,
                None => continue,
            },
            WidthTie::Conversion(Conversion::Widen | Conversion::Narrow)
            | WidthTie::Free
            | WidthTie::NoHead => continue,
        };
        if got.bit_width() != want.bit_width() {
            errs.push(ValidationError::ArithmeticWidthMismatch {
                node,
                kind: *kind,
                operand_idx: slot,
                operand_width: got.bit_width(),
                expected_width: want.bit_width(),
            });
        }
    }
}

/// `Extend` must strictly widen its input; `Truncate` must strictly narrow it.
pub(super) fn check_extend_truncate(
    function: &Function,
    node: NodeId,
    kind: &NodeKind,
    errs: &mut Vec<ValidationError>,
) {
    let graph = function.graph();
    let conversion = match kind.operand_width(0) {
        OperandWidth::Conversion(c @ (Conversion::Widen | Conversion::Narrow)) => c,
        OperandWidth::Conversion(Conversion::SameSize)
        | OperandWidth::OfOutput
        | OperandWidth::OfFirstOperand
        | OperandWidth::Free => return,
    };
    let (Some(in_ty), Some(out_ty)) = (
        graph
            .node_inputs(node)
            .into_iter()
            .next()
            .and_then(|v| function.value_type_opt(v)),
        first_output_type(graph, node),
    ) else {
        return;
    };
    let (in_width, out_width) = (in_ty.bit_width(), out_ty.bit_width());
    if !conversion.admits(in_ty, out_ty) {
        errs.push(ValidationError::ExtendTruncateWidthDirection {
            node,
            kind: *kind,
            in_width,
            out_width,
        });
    }
}

/// Every reachable [`NodeKind::Switch`] needs a table of this function's, at
/// least one control output, and one case address per control output.
pub(super) fn check_switch(
    function: &Function,
    node: NodeId,
    kind: &NodeKind,
    errs: &mut Vec<ValidationError>,
) {
    let graph = function.graph();
    let NodeKind::Switch(id) = *kind else {
        return;
    };
    let Some(targets) = function.switch_table(id) else {
        errs.push(ValidationError::DanglingSwitchTableId { node, id });
        return;
    };
    let n_out = graph.node_outputs(node).len();
    if n_out == 0 {
        // Never the only error: `Switch`'s signature head is one Control,
        // so `local_typing`'s arity check reports
        // `NodeOutputCountMismatch` for the same node in the same call.
        // Kept because it names WHICH arity rule a zero-arm `Switch`
        // breaks, which the generic count does not.
        errs.push(ValidationError::EmptySwitchTargets { node });
    } else if n_out != targets.len() {
        errs.push(ValidationError::SwitchTargetArityMismatch {
            node,
            outputs: n_out,
            targets: targets.len(),
        });
    }
}

/// Every reachable [`NodeKind::Call`] override names a convention of this
/// function's.
pub(super) fn check_call_cc(
    function: &Function,
    node: NodeId,
    kind: &NodeKind,
    errs: &mut Vec<ValidationError>,
) {
    if let NodeKind::Call { cc: Some(id) } = *kind
        && function.cc(id).is_none()
    {
        errs.push(ValidationError::DanglingCcId { node, id });
    }
}

/// Every `Phi` / `MemPhi` takes its dispatch token (`input[0]`) from a
/// `Region`'s `PhiToken` output, and has one value input per predecessor of
/// that owning `Region`.
pub(super) fn check_phis(
    function: &Function,
    node: NodeId,
    kind: &NodeKind,
    errs: &mut Vec<ValidationError>,
) {
    let graph = function.graph();
    let is_phi = matches!(kind, NodeKind::Phi | NodeKind::MemPhi);
    if !is_phi {
        return;
    }

    let inputs = graph.node_inputs(node);
    if inputs.is_empty() {
        // Local typing already reports empty-input phis.
        return;
    }
    let token = inputs[NodeKind::PHI_TOKEN_SLOT];
    let token_kind = graph.value_kind(token);
    let owner = graph.producer(token);
    if token_kind != ValueKind::PhiToken || !matches!(graph.node_kind(owner), NodeKind::Region) {
        errs.push(ValidationError::PhiTokenNotFromRegion {
            phi: node,
            producer: owner,
            producer_kind: token_kind,
        });
        return;
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

/// Every reachable, non-exempt node carries at least one asm-fingerprint
/// contributor.
pub(super) fn check_asm_fingerprint(
    function: &Function,
    node: NodeId,
    kind: &NodeKind,
    errs: &mut Vec<ValidationError>,
) {
    if kind.asm_fingerprint_exempt() {
        return;
    }
    if function.side_tables().asm_fingerprint_is_empty(node) {
        errs.push(ValidationError::MissingAsmFingerprint { node, kind: *kind });
    }
}

/// Every reachable `Store` has its Memory output consumed by a reachable node.
///
/// Scoped to `Store`: a memory-preserving `Call` / `CallOther` legitimately
/// leaves its Memory output unconsumed.
///
/// The entry walk reaches a `Store` only through a consumer of that output, so
/// a violation means a severed use-list. The stronger shape, every `Store` the
/// memory chain reaches is entry-reachable, is false: `PhiCollapse` can make
/// two stores identical, and the node cache subsumes one into the other,
/// leaving a corpse that still consumes the chain.
pub(super) fn check_memory_chain(
    function: &Function,
    reachable: &NodeIdSet,
    node: NodeId,
    kind: &NodeKind,
    errs: &mut Vec<ValidationError>,
) {
    let graph = function.graph();
    if !matches!(kind, NodeKind::Store(_)) {
        return;
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

/// Every reachable `InitialVar(id)` names a minted varnode and is typed as the
/// integer of that varnode's size.
pub(super) fn check_initial_vars(
    function: &Function,
    node: NodeId,
    kind: &NodeKind,
    errs: &mut Vec<ValidationError>,
) {
    let graph = function.graph();
    let NodeKind::InitialVar(id) = *kind else {
        return;
    };
    let Some(vn) = function.initial_vn_opt(id) else {
        errs.push(ValidationError::DanglingInitialVnId { node, id });
        return;
    };
    let Some(ty) = first_output_type(graph, node) else {
        return; // arity reported elsewhere
    };
    if ValueType::int_from_bytes(vn.size).ok() != Some(ty) {
        errs.push(ValidationError::InitialVarTypeMismatch { node, vn, ty });
    }
}

/// The side-indices must not have drifted from the live graph.
///
/// * Every reachable `initial_var_index` entry resolves to an
///   `InitialVar(vn)` node with the same varnode. Unreachable entries are
///   tolerated.
/// * Every `value_vn` key with a reachable producer is produced by a `Phi` /
///   `Call` / `CallOther`.
///
/// An entry keyed by an unminted id is a `DanglingInitialVnId`.
pub(super) fn check_function_invariants_side_indices(
    function: &Function,
    reachable: &NodeIdSet,
    errs: &mut Vec<ValidationError>,
) {
    let graph = function.graph();
    for (id, node) in function.initial_var_index_entries() {
        if !reachable.contains(node) {
            continue;
        }
        let Some(vn) = function.initial_vn_opt(id) else {
            errs.push(ValidationError::DanglingInitialVnId { node, id });
            continue;
        };
        let kind = graph.node_kind(node);
        if !matches!(kind, NodeKind::InitialVar(found) if *found == id) {
            errs.push(ValidationError::StaleInitialVarIndex {
                node,
                vn,
                actual_kind: *kind,
            });
        }
    }

    for (value, id) in function.value_vn_entries() {
        let producer = graph.producer(value);
        if !reachable.contains(producer) {
            continue;
        }
        let Some(vn) = function.initial_vn_opt(id) else {
            errs.push(ValidationError::DanglingInitialVnId { node: producer, id });
            continue;
        };
        let producer_kind = graph.node_kind(producer);
        if !matches!(
            producer_kind,
            NodeKind::Phi | NodeKind::Call { .. } | NodeKind::CallOther { .. }
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
/// value fits the node's declared output width, and every reachable
/// `FloatConst` is declared at most 8 bytes wide and carries no bits above
/// that width.
pub(super) fn check_consts(
    function: &Function,
    node: NodeId,
    kind: &NodeKind,
    errs: &mut Vec<ValidationError>,
) {
    let graph = function.graph();
    if let NodeKind::FloatConst(bits) = *kind {
        let Some(ty) = first_output_type(graph, node) else {
            return; // arity reported elsewhere
        };
        if ty.is_float() {
            if ty.byte_size() > 8 {
                errs.push(ValidationError::FloatConstUnrepresentableType { node, ty });
            } else if ty.mask_float_bits(bits) != bits {
                errs.push(ValidationError::FloatConstWidthMismatch { node, bits });
            }
        }
        return;
    }
    let NodeKind::IntConst(id) = *kind else {
        return;
    };
    let Some(value) = function.side_tables().const_interner.get(id) else {
        errs.push(ValidationError::DanglingConstId { node, id });
        return;
    };
    let Some(ty) = first_output_type(graph, node) else {
        return; // arity reported elsewhere
    };
    // Canonical masking: every bit above the declared width is zero.
    if *value != crate::eval::mask(value, ty) {
        errs.push(ValidationError::ConstWidthMismatch { node, id });
    }
}

/// Every control-reachable node must reach a terminator. The use-count checks
/// pass an exit-free control cycle: its back-edge consumes the header's control
/// output, so nothing there is unused or reused, yet no terminator exists to
/// anchor liveness and compaction drops the whole body.
pub(super) fn check_function_invariants_terminator_reachable(
    function: &Function,
    errs: &mut Vec<ValidationError>,
) {
    let stranded = crate::walk::stranded_nodes(function.graph(), function.entry());
    let mut stranded = stranded.iter();
    if let Some(node) = stranded.next() {
        errs.push(ValidationError::NoTerminatorReachable {
            node,
            count: 1 + stranded.count(),
        });
    }
}

/// Every live `Store` is on a memory chain that reaches a sink: the memory
/// input of a live `Return`, `IndirectBranch`, `Unreachable`, `Call` or
/// `CallOther`. The chain steps back through `Store`, the live-edge arms of a
/// `MemPhi` and `Call` / `CallOther` memory inputs. A
/// `Load` is no sink, so a write only a `Load` reads, while the chain carries
/// on from the `Store`'s own input token, is lost. A token may fork: each arm
/// of a branch continues the same chain.
pub(super) fn check_function_invariants_memory_linearity(
    ctx: &ScheduleContext<'_>,
    errs: &mut Vec<ValidationError>,
) {
    let function = ctx.function;
    let graph = function.graph();
    // Of the control nodes, exactly the memory sinks take Memory there.
    let mut stack: Vec<ValueId> = ctx
        .domtree
        .preorder()
        .iter()
        .filter_map(|&node| ctrl_memory_input(graph, node))
        .collect();
    let mut on_chain = NodeIdSet::new();
    while let Some(value) = stack.pop() {
        let producer = graph.producer(value);
        if !on_chain.insert(producer) {
            continue;
        }
        if matches!(graph.node_kind(producer), NodeKind::MemPhi) {
            stack.extend(live_arms(function, &ctx.domtree, producer));
        } else {
            stack.extend(function.memory_input_of(producer));
        }
    }
    for node in &ctx.live {
        if matches!(graph.node_kind(node), NodeKind::Store(_)) && !on_chain.contains(node) {
            errs.push(ValidationError::LostStore { node });
        }
    }
    check_memory_order(ctx, &on_chain, errs);
}

/// Memory state follows control: every PINNED read of it takes the latest
/// memory effect on each path in.
///
/// A `Load` is unpinned, so its own memory input is not judged here: a `Load`
/// on an earlier token legitimately denotes the pre-store value, and the case
/// where that leaves a write unobserved is `LostStore`'s.
///
/// A control node's state is the nearest memory effect dominating it: the
/// entry, a `Region` with a `MemPhi` on the chain, a `Call` whose convention
/// does not preserve memory, or a `Call` / `CallOther` on the chain. A memory
/// input steps back through `Store`s to one of these, which must be the state
/// before its consumer. A `MemPhi` arm must be its edge's state, and at a
/// `Region` with no such `MemPhi` every live edge carries the state the
/// `Region`'s immediate dominator has.
fn check_memory_order(
    ctx: &ScheduleContext<'_>,
    on_chain: &NodeIdSet,
    errs: &mut Vec<ValidationError>,
) {
    let function = ctx.function;
    let graph = function.graph();
    let tree = &ctx.domtree;
    let chain_mem_phis = |region: NodeId| {
        ctx.attached_phis(region).filter(|&phi| {
            matches!(graph.node_kind(phi), NodeKind::MemPhi) && on_chain.contains(phi)
        })
    };
    let is_effect = |node: NodeId| match graph.node_kind(node) {
        NodeKind::Entry => true,
        NodeKind::Region => chain_mem_phis(node).next().is_some(),
        NodeKind::Call { .. } => !function.get_cc(node).preserves_memory || on_chain.contains(node),
        NodeKind::CallOther { .. } => on_chain.contains(node),
        NodeKind::InitialMemory
        | NodeKind::InitialVar(_)
        | NodeKind::MemPhi
        | NodeKind::Phi
        | NodeKind::If
        | NodeKind::Switch(_)
        | NodeKind::Return
        | NodeKind::IndirectBranch
        | NodeKind::Unreachable
        | NodeKind::Load(_)
        | NodeKind::Store(_)
        | NodeKind::IntConst(_)
        | NodeKind::IntUnaryOp(_)
        | NodeKind::IntBinaryOp(_)
        | NodeKind::IntCmpOp(_)
        | NodeKind::Truncate
        | NodeKind::Popcount
        | NodeKind::Lzcount
        | NodeKind::Extend(_)
        | NodeKind::FloatConst(_)
        | NodeKind::FloatBinaryOp(_)
        | NodeKind::FloatUnaryOp(_)
        | NodeKind::FloatCmpOp(_)
        | NodeKind::IntToFloat
        | NodeKind::FloatToInt
        | NodeKind::FloatToFloat
        | NodeKind::IntBitsToFloat
        | NodeKind::FloatBitsToInt
        | NodeKind::CPoolRef
        | NodeKind::New => false,
    };

    // The effect a memory token was last advanced by, memoized per `Store`.
    let mut store_origin: SecondaryMap<NodeId, Option<NodeId>> = SecondaryMap::new();
    let mut stores: Vec<NodeId> = Vec::new();
    let mut origin = |value: ValueId| {
        let mut producer = graph.producer(value);
        let found = loop {
            match graph.node_kind(producer) {
                NodeKind::Store(_) => {
                    if let Some(known) = store_origin[producer] {
                        break known;
                    }
                    stores.push(producer);
                    producer = graph.producer(
                        graph
                            .nth_input(producer, NodeKind::ACCESS_MEM_SLOT)
                            .expect("a Store has memory"),
                    );
                }
                NodeKind::MemPhi => break function.phi_region(producer),
                NodeKind::InitialMemory => break tree.root(),
                NodeKind::Entry
                | NodeKind::InitialVar(_)
                | NodeKind::Region
                | NodeKind::Phi
                | NodeKind::If
                | NodeKind::Switch(_)
                | NodeKind::Call { .. }
                | NodeKind::Return
                | NodeKind::IndirectBranch
                | NodeKind::Unreachable
                | NodeKind::Load(_)
                | NodeKind::IntConst(_)
                | NodeKind::IntUnaryOp(_)
                | NodeKind::IntBinaryOp(_)
                | NodeKind::IntCmpOp(_)
                | NodeKind::Truncate
                | NodeKind::Popcount
                | NodeKind::Lzcount
                | NodeKind::Extend(_)
                | NodeKind::FloatConst(_)
                | NodeKind::FloatBinaryOp(_)
                | NodeKind::FloatUnaryOp(_)
                | NodeKind::FloatCmpOp(_)
                | NodeKind::IntToFloat
                | NodeKind::FloatToInt
                | NodeKind::FloatToFloat
                | NodeKind::IntBitsToFloat
                | NodeKind::FloatBitsToInt
                | NodeKind::CallOther { .. }
                | NodeKind::CPoolRef
                | NodeKind::New => break producer,
            }
        };
        for store in stores.drain(..) {
            store_origin[store] = Some(found);
        }
        found
    };

    // Pre-order with a stack of ancestors: the top that dominates a node is its
    // immediate dominator.
    let mut state: SecondaryMap<NodeId, Option<NodeId>> = SecondaryMap::new();
    let mut idom: SecondaryMap<NodeId, Option<NodeId>> = SecondaryMap::new();
    let mut ancestors: Vec<NodeId> = Vec::new();
    for &node in tree.preorder() {
        while ancestors.last().is_some_and(|&a| !tree.dominates(a, node)) {
            ancestors.pop();
        }
        let parent = ancestors.last().copied();
        idom[node] = parent;
        let before = parent.and_then(|p| state[p]);
        state[node] = if is_effect(node) { Some(node) } else { before };
        ancestors.push(node);

        if let (Some(memory), Some(effect)) = (ctrl_memory_input(graph, node), before)
            && origin(memory) != effect
        {
            errs.push(ValidationError::MemoryRewind { node, effect });
        }
    }

    for &region in tree.preorder() {
        if !matches!(graph.node_kind(region), NodeKind::Region) {
            continue;
        }
        let edge_states = graph
            .node_inputs(region)
            .into_iter()
            .map(|edge| state[graph.producer(edge)]);
        if is_effect(region) {
            for phi in chain_mem_phis(region) {
                let arms = graph.node_inputs(phi).into_iter().skip(1);
                for (arm, edge_state) in arms.zip(edge_states.clone()) {
                    if let Some(effect) = edge_state
                        && origin(arm) != effect
                    {
                        errs.push(ValidationError::MemoryRewind { node: phi, effect });
                    }
                }
            }
        } else {
            let merged = idom[region].and_then(|d| state[d]);
            for effect in edge_states.flatten() {
                if Some(effect) != merged {
                    errs.push(ValidationError::UnmergedMemoryEffect { region, effect });
                }
            }
        }
    }
}

/// A control node's memory input; of the control nodes, exactly the memory
/// readers take Memory at [`NodeKind::CTRL_MEM_SLOT`].
fn ctrl_memory_input(graph: &Graph, node: NodeId) -> Option<ValueId> {
    graph
        .nth_input(node, NodeKind::CTRL_MEM_SLOT)
        .filter(|&v| graph.value_kind(v) == ValueKind::Memory)
}

/// A phi's value inputs on edges from a node of `tree`.
fn live_arms<'a>(
    function: &'a Function,
    tree: &'a DominatorTree<NodeId>,
    phi: NodeId,
) -> impl Iterator<Item = ValueId> + 'a {
    function
        .phi_arms(phi)
        .filter(|&(edge, _)| tree.contains(function.producer(edge)))
        .map(|(_, arm)| arm)
}

/// Every data input is available where it is used, judged on an early schedule
/// ([`schedule_early`]): a floating node sits at the latest of its inputs'
/// positions, which must lie on one dominator chain; a control node's latest
/// input must dominate it; a phi's input must dominate the predecessor edge it
/// arrives on. An input with no position at all, computed only in unreachable
/// code or on a data cycle, is unavailable to a live use. A phi input on an
/// unreachable edge can never be selected, and is not judged.
pub(super) fn check_function_invariants_availability(
    ctx: &ScheduleContext<'_>,
    errs: &mut Vec<ValidationError>,
) {
    let graph = ctx.function.graph();
    let tree = &ctx.domtree;

    let mut at: SecondaryMap<NodeId, Option<NodeId>> = SecondaryMap::new();
    for &cfg_node in tree.preorder() {
        at[cfg_node] = Some(cfg_node);
        for phi in ctx.attached_phis(cfg_node) {
            at[phi] = Some(cfg_node);
        }
    }

    schedule_early(ctx, |node| {
        let latest = latest_input(graph, tree, &at, node, errs);
        at[node] = Some(latest.map_or(tree.root(), |(pos, _)| pos));
    });

    for &cfg_node in tree.preorder() {
        if let Some((pos, input_idx)) = latest_input(graph, tree, &at, cfg_node, errs)
            && !tree.dominates(pos, cfg_node)
        {
            errs.push(ValidationError::InputNotAvailable {
                node: cfg_node,
                input_idx,
            });
        }
        let preds = graph.node_inputs(cfg_node);
        for phi in ctx.attached_phis(cfg_node) {
            let arms = graph.node_inputs(phi).into_iter().skip(1);
            for ((input_idx, value), edge) in (1..).zip(arms).zip(preds) {
                let Some(edge_at) = at[graph.producer(edge)] else {
                    continue;
                };
                if !at[graph.producer(value)].is_some_and(|pos| tree.dominates(pos, edge_at)) {
                    errs.push(ValidationError::InputNotAvailable {
                        node: phi,
                        input_idx,
                    });
                }
            }
        }
    }
}

/// The dominance-latest position among `node`'s data inputs and the input
/// holding it. An input with no position, or one off the chain the earlier
/// inputs lie on, is reported; the latter ends the scan.
fn latest_input(
    graph: &Graph,
    tree: &DominatorTree<NodeId>,
    at: &SecondaryMap<NodeId, Option<NodeId>>,
    node: NodeId,
    errs: &mut Vec<ValidationError>,
) -> Option<(NodeId, usize)> {
    let mut latest: Option<(NodeId, usize)> = None;
    for (input_idx, value) in graph.node_inputs(node).into_iter().enumerate() {
        if graph.value_kind(value).is_control() {
            continue;
        }
        let Some(pos) = at[graph.producer(value)] else {
            errs.push(ValidationError::InputNotAvailable { node, input_idx });
            continue;
        };
        match latest {
            Some((cur, _)) if tree.dominates(pos, cur) => {}
            Some((cur, _)) if !tree.dominates(cur, pos) => {
                errs.push(ValidationError::InputNotAvailable { node, input_idx });
                return latest;
            }
            _ => latest = Some((pos, input_idx)),
        }
    }
    latest
}
