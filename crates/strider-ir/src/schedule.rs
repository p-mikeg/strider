//! Placement of floating nodes on the control dominator tree.
//!
//! A control node is pinned where it is, and a phi at the `Region` that owns
//! it. Every other node floats: [`schedule_early`] visits each one after all
//! of its floating data producers, so a caller can place it from its inputs'
//! positions.

use crate::DominatorTree;
use crate::IRViewer;
use crate::function::Function;
use crate::graph::Graph;
use crate::node::{NodeId, NodeKind, ValueKind};
use crate::walk::{NodeIdSet, PostOrder};

/// A control node or a phi: its position is fixed by the control flow rather
/// than chosen from its inputs.
pub(crate) fn is_pinned(graph: &Graph, node: NodeId) -> bool {
    let kind = graph.node_kind(node);
    kind.has_control_flow() || matches!(kind, NodeKind::Phi | NodeKind::MemPhi)
}

pub(crate) struct ScheduleContext<'a> {
    pub(crate) function: &'a Function,
    /// What a control node of the dominator tree can use on a path that runs:
    /// its data producers and its phis', transitively, except a phi input on an
    /// edge from a node outside the tree, which is never selected.
    pub(crate) live: NodeIdSet,
    pub(crate) domtree: DominatorTree<NodeId>,
}

impl<'a> ScheduleContext<'a> {
    /// Over `reachable`, the nodes a walk from the entry reaches.
    pub(crate) fn new(function: &'a Function, reachable: &NodeIdSet) -> Self {
        let domtree = crate::control_dominator_tree(function);
        let live = cfg_live(function.graph(), reachable, &domtree);
        Self {
            function,
            live,
            domtree,
        }
    }

    /// The live phis owned by `cfg_node`, a `Region`; none for any other kind.
    pub(crate) fn attached_phis(&self, cfg_node: NodeId) -> impl Iterator<Item = NodeId> + '_ {
        region_phis(self.function.graph(), cfg_node).filter(|&phi| self.live.contains(phi))
    }

    /// Each control node of the dominator tree in pre-order, followed by its
    /// attached phis.
    pub(crate) fn pinned_nodes(&self) -> impl Iterator<Item = NodeId> + '_ {
        self.domtree
            .preorder()
            .iter()
            .flat_map(|&cfg_node| core::iter::once(cfg_node).chain(self.attached_phis(cfg_node)))
    }
}

/// The phis owned by `cfg_node`, a `Region`; none for any other kind.
fn region_phis(graph: &Graph, cfg_node: NodeId) -> impl Iterator<Item = NodeId> + '_ {
    graph
        .node_outputs(cfg_node)
        .iter()
        .copied()
        .filter(|&v| matches!(graph.value_kind(v), ValueKind::PhiToken))
        .flat_map(|token| graph.value_uses(token))
        .map(|(phi, _slot)| phi)
}

/// [`ScheduleContext::live`]: seeded with every tree node and its reachable
/// phis, closed over data inputs, a phi's input `i` followed only when its
/// region's control input `i - 1` comes from a tree node.
fn cfg_live(graph: &Graph, reachable: &NodeIdSet, tree: &DominatorTree<NodeId>) -> NodeIdSet {
    let mut live = NodeIdSet::new();
    let mut stack: Vec<NodeId> = Vec::new();
    for &cfg_node in tree.preorder() {
        stack.push(cfg_node);
        stack.extend(region_phis(graph, cfg_node).filter(|&phi| reachable.contains(phi)));
    }
    while let Some(node) = stack.pop() {
        if !live.insert(node) {
            continue;
        }
        let inputs = graph.node_inputs(node);
        let region_preds = matches!(graph.node_kind(node), NodeKind::Phi | NodeKind::MemPhi)
            .then(|| inputs.into_iter().next())
            .flatten()
            .map(|token| graph.node_inputs(graph.value_definition(token).0));
        for (i, value) in inputs.into_iter().enumerate() {
            if graph.value_kind(value).is_control() {
                continue;
            }
            if let (Some(preds), Some(edge_idx)) = (&region_preds, i.checked_sub(1))
                && let Some(&edge) = preds.get(edge_idx)
                && !tree.contains(graph.value_definition(edge).0)
            {
                continue;
            }
            let producer = graph.value_definition(value).0;
            if reachable.contains(producer) {
                stack.push(producer);
            }
        }
    }
    live
}

/// The live floating data producers of a node.
#[derive(Clone, Copy)]
struct UnpinnedDataPreds<'a> {
    graph: &'a Graph,
    live: &'a NodeIdSet,
}

impl UnpinnedDataPreds<'_> {
    fn of(self, node: NodeId) -> impl Iterator<Item = NodeId> {
        self.graph
            .node_inputs(node)
            .into_iter()
            .filter(move |&v| !self.graph.value_kind(v).is_control())
            .map(move |v| self.graph.value_definition(v).0)
            .filter(move |&n| self.live.contains(n) && !is_pinned(self.graph, n))
    }
}

impl graph_algorithms::walk::GraphRef for UnpinnedDataPreds<'_> {
    type NodeId = NodeId;

    fn successors(&self, node: NodeId, mut f: impl FnMut(NodeId)) {
        for succ in self.of(node) {
            f(succ);
        }
    }
}

/// Calls `schedule` once for every floating node that feeds a pinned node,
/// after every floating producer of it that is not on a data cycle with it.
pub(crate) fn schedule_early(ctx: &ScheduleContext<'_>, mut schedule: impl FnMut(NodeId)) {
    let preds = UnpinnedDataPreds {
        graph: ctx.function.graph(),
        live: &ctx.live,
    };
    let roots: Vec<NodeId> = ctx.pinned_nodes().flat_map(|p| preds.of(p)).collect();
    for node in PostOrder::new(preds, roots) {
        schedule(node);
    }
}

/// Where `value` is first computable on `tree`, the function's
/// [`crate::control_dominator_tree`]: a control node at itself, a phi at its
/// `Region`, and a floating node at the latest of its data inputs' positions,
/// the tree root when it has none. `None` when a position is outside the tree
/// or two inputs lie on different dominator branches.
pub fn early_position(
    function: &Function,
    tree: &DominatorTree<NodeId>,
    value: crate::node::ValueId,
) -> Option<NodeId> {
    let graph = function.graph();
    let pinned_at = |node: NodeId| -> Option<Option<NodeId>> {
        let kind = graph.node_kind(node);
        let at = if matches!(kind, NodeKind::Phi | NodeKind::MemPhi) {
            function.phi_region(node)
        } else if kind.has_control_flow() {
            node
        } else {
            return None;
        };
        Some(tree.contains(at).then_some(at))
    };
    let data_preds = |node: NodeId| {
        graph
            .node_inputs(node)
            .into_iter()
            .filter(|&v| !graph.value_kind(v).is_control())
            .map(|v| graph.value_definition(v).0)
    };
    let mut at: rustc_hash::FxHashMap<NodeId, Option<NodeId>> = Default::default();
    let mut stack = vec![(graph.value_definition(value).0, false)];
    while let Some((node, expanded)) = stack.pop() {
        if expanded {
            let mut latest = Some(tree.root());
            for pred in data_preds(node) {
                latest = match (latest, at.get(&pred).copied().flatten()) {
                    (Some(cur), Some(pos)) if tree.dominates(cur, pos) => Some(pos),
                    (Some(cur), Some(pos)) if tree.dominates(pos, cur) => Some(cur),
                    _ => None,
                };
            }
            at.insert(node, latest);
            continue;
        }
        if at.contains_key(&node) {
            continue;
        }
        if let Some(pos) = pinned_at(node) {
            at.insert(node, pos);
            continue;
        }
        // In progress: a data cycle back to it reads no position.
        at.insert(node, None);
        stack.push((node, true));
        stack.extend(data_preds(node).map(|p| (p, false)));
    }
    at[&graph.value_definition(value).0]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::function::test_function;
    use crate::node::signature::every_node_kind;
    use crate::node::{ExpectedValueKind as E, ValueType};

    /// On a node whose edges fill its signature, at its shortest and one
    /// past, `is_pinned` is "a control kind or a phi".
    #[test]
    fn is_pinned_is_control_flow_or_phi_on_signature_conforming_nodes() {
        let concrete = |e: E| match e {
            E::Control => ValueKind::Control,
            E::Memory => ValueKind::Memory,
            E::PhiToken => ValueKind::PhiToken,
            E::Bool => ValueKind::Typed(ValueType::I1),
            E::AnyInt | E::AnyValue => ValueKind::Typed(ValueType::int(32)),
            E::AnyFloat => ValueKind::Typed(ValueType::float(64)),
        };
        let all = [
            E::Control,
            E::Memory,
            E::PhiToken,
            E::Bool,
            E::AnyInt,
            E::AnyFloat,
            E::AnyValue,
        ];
        let mut function = test_function();
        let graph = function.graph_mut();
        let source = graph.create_node(NodeKind::New, [], all.map(concrete));
        let sources: Vec<_> = graph.node_outputs(source).to_vec();
        let value_of = |e: E| sources[all.iter().position(|&a| a == e).unwrap()];
        let lens = |head: usize, at: &dyn Fn(usize) -> Option<E>| {
            let extra = at(head).is_some();
            [Some(head), extra.then_some(head + 1)]
                .into_iter()
                .flatten()
        };
        let mut checked = 0;
        for kind in every_node_kind() {
            let input_at = |i| kind.expected_input_kind(i);
            let output_at = |i| kind.expected_output_kind(i);
            for n_in in lens(kind.input_head_len(), &input_at) {
                for n_out in lens(kind.output_head_len(), &output_at) {
                    let inputs: Vec<_> =
                        (0..n_in).map(|i| value_of(input_at(i).unwrap())).collect();
                    let outputs: Vec<_> = (0..n_out)
                        .map(|i| concrete(output_at(i).unwrap()))
                        .collect();
                    let node = graph.create_node(kind, inputs, outputs);
                    let phi = matches!(kind, NodeKind::Phi | NodeKind::MemPhi);
                    assert_eq!(
                        is_pinned(graph, node),
                        kind.has_control_flow() || phi,
                        "{kind:?} with {n_in} inputs, {n_out} outputs"
                    );
                    checked += 1;
                }
            }
        }
        assert!(checked > every_node_kind().len());
    }
}
