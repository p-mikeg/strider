//! Placement of floating nodes on the control dominator tree.
//!
//! A control node is pinned where it is, and a phi at the `Region` that owns
//! it. Every other node floats: [`schedule_early`] visits each one after all
//! of its floating data producers, so a caller can place it from its inputs'
//! positions.

use core::ops::ControlFlow;

use cranelift_entity::{EntityRef, SecondaryMap};

use crate::function::Function;
use crate::graph::Graph;
use crate::node::{NodeId, NodeKind, ValueKind};
use crate::walk::{NodeIdSet, PostOrder, WalkPhase};

/// The control dominator tree, numbered by pre/post interval so
/// [`Self::dominates`] is O(1).
pub(crate) struct DomTree {
    /// `(0, 0)` for a node outside the tree.
    span: SecondaryMap<NodeId, (u32, u32)>,
    preorder: Vec<NodeId>,
}

impl DomTree {
    /// Over the control nodes reachable from `function`'s entry; `live` is the
    /// universe the tree's children are read from.
    pub(crate) fn compute(function: &Function, live: &NodeIdSet) -> Self {
        let doms = crate::control_flow_view::control_dominators(function);
        let mut edges: Vec<(NodeId, NodeId)> = live
            .iter()
            .filter_map(|n| doms.immediate_dominator(n).map(|d| (d, n)))
            .collect();
        edges.sort_unstable_by_key(|&(d, _)| d.index());
        let mut span: SecondaryMap<NodeId, (u32, u32)> = SecondaryMap::new();
        let mut preorder = Vec::new();
        let mut clock = 1u32;
        let mut stack = vec![(function.entry(), false)];
        while let Some((node, done)) = stack.pop() {
            if done {
                span[node].1 = clock;
            } else {
                span[node].0 = clock;
                preorder.push(node);
                stack.push((node, true));
                let lo = edges.partition_point(|&(d, _)| d.index() < node.index());
                stack.extend(
                    edges[lo..]
                        .iter()
                        .take_while(|&&(d, _)| d == node)
                        .map(|&(_, child)| (child, false)),
                );
            }
            clock += 1;
        }
        Self { span, preorder }
    }

    pub(crate) fn root(&self) -> NodeId {
        self.preorder[0]
    }

    /// Control nodes in dominator-tree pre-order, so a node follows every node
    /// that dominates it.
    pub(crate) fn preorder(&self) -> &[NodeId] {
        &self.preorder
    }

    pub(crate) fn contains(&self, node: NodeId) -> bool {
        self.span[node].0 != 0
    }

    pub(crate) fn dominates(&self, a: NodeId, b: NodeId) -> bool {
        let ((a_pre, a_post), (b_pre, b_post)) = (self.span[a], self.span[b]);
        a_pre != 0 && b_pre != 0 && a_pre <= b_pre && b_post <= a_post
    }
}

/// A node with a control input or output, or a phi: its position is fixed by
/// the control flow rather than chosen from its inputs.
pub(crate) fn is_pinned(graph: &Graph, node: NodeId) -> bool {
    matches!(graph.node_kind(node), NodeKind::Phi | NodeKind::MemPhi)
        || graph
            .node_inputs(node)
            .into_iter()
            .any(|v| graph.value_kind(v).is_control())
        || graph
            .node_outputs(node)
            .iter()
            .any(|&v| graph.value_kind(v).is_control())
}

pub(crate) struct ScheduleContext<'a> {
    pub(crate) function: &'a Function,
    /// What a control node of the dominator tree can use on a path that runs:
    /// its data producers and its phis', transitively, except a phi input on an
    /// edge from a node outside the tree, which is never selected.
    pub(crate) live: NodeIdSet,
    pub(crate) domtree: DomTree,
}

impl<'a> ScheduleContext<'a> {
    /// Over `reachable`, the nodes a walk from the entry reaches.
    pub(crate) fn new(function: &'a Function, reachable: &NodeIdSet) -> Self {
        let domtree = DomTree::compute(function, reachable);
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
fn cfg_live(graph: &Graph, reachable: &NodeIdSet, tree: &DomTree) -> NodeIdSet {
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
                && let Some(edge) = preds.into_iter().nth(edge_idx)
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

    fn try_successors(
        &self,
        node: NodeId,
        f: impl FnMut(NodeId) -> ControlFlow<()>,
    ) -> ControlFlow<()> {
        self.of(node).try_for_each(f)
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
    let mut walk = PostOrder::new(preds, roots);
    while let Some((phase, node)) = walk.next_event() {
        if matches!(phase, WalkPhase::Post) {
            schedule(node);
        }
    }
}
