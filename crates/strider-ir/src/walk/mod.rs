use core::iter;

use entity_utils::DenseEntitySet;

use crate::IRViewer;
use crate::function::Function;
use crate::graph::Graph;
use crate::node::{NodeId, NodeKind, ValueId};

mod cast;
pub use cast::{CastMask, cast_mask_of};
#[cfg(test)]
mod tests;

pub type NodeIdSet = DenseEntitySet<NodeId>;

/// The CFG skeleton: only `Control` edges are followed, so the result holds
/// control-flow nodes alone.
pub fn cfg_reachable(graph: &Graph, entry: NodeId) -> DenseEntitySet<NodeId> {
    let mut walk = PreOrder::new(FnSuccs::new(|n| cfg_succs(graph, n)), iter::once(entry));
    walk.by_ref().for_each(drop);
    walk.into_visited()
}

/// Control-reachable from `entry` and unable to reach a terminator: an
/// exit-free control cycle plus everything that only reaches it. A dangling
/// control output counts as an exit.
pub fn stranded_nodes(graph: &Graph, entry: NodeId) -> NodeIdSet {
    let cfg = cfg_reachable(graph, entry);

    let mut escapes = NodeIdSet::new();
    let mut work: Vec<NodeId> = Vec::new();
    for node in &cfg {
        let dangling =
            cfg_outputs(graph, node).any(|value| graph.value_uses(value).next().is_none());
        if graph.node_kind(node).is_terminator() || dangling {
            escapes.insert(node);
            work.push(node);
        }
    }
    close_over_control_preds(graph, &mut escapes, work, |_| false, Some(&cfg));

    cfg.iter().filter(|&node| !escapes.contains(node)).collect()
}

/// Grows `reached` backward over control inputs from `work`.
///
/// `skip_edge` drops an edge that must not be followed (a branch arm proven
/// dead); `universe`, when given, bounds membership.
pub fn close_over_control_preds(
    graph: &Graph,
    reached: &mut NodeIdSet,
    mut work: Vec<NodeId>,
    skip_edge: impl Fn(ValueId) -> bool,
    universe: Option<&NodeIdSet>,
) {
    while let Some(node) = work.pop() {
        for value in graph.node_inputs(node) {
            if !graph.value_kind(value).is_control() || skip_edge(value) {
                continue;
            }
            let pred = graph.value_definition(value).0;
            if universe.is_none_or(|u| u.contains(pred)) && reached.insert(pred) {
                work.push(pred);
            }
        }
    }
}

pub use graph_algorithms::walk::{FnSuccs, PostOrder, PreOrder, WalkPhase};

/// Successors follow every input backward and control edges forward.
#[derive(Clone, Copy)]
pub struct GraphWalkSuccs<'a>(&'a Graph);

pub fn cfg_outputs(graph: &Graph, node: NodeId) -> impl Iterator<Item = ValueId> + '_ {
    graph
        .node_outputs(node)
        .iter()
        .copied()
        .filter(|&output| graph.value_kind(output).is_control())
}

pub(crate) fn cfg_succs(graph: &Graph, node: NodeId) -> impl Iterator<Item = NodeId> + '_ {
    cfg_outputs(graph, node)
        .flat_map(|output| graph.value_uses(output))
        .map(|(succ_node, _succ_input_idx)| succ_node)
}

/// Forward def-use successors, dead consumers included.
pub(crate) fn def_use_succs(graph: &Graph, node: NodeId) -> impl Iterator<Item = NodeId> + '_ {
    graph
        .node_outputs(node)
        .iter()
        .flat_map(move |output| graph.value_uses(*output))
        .map(|(succ, _use_idx)| succ)
}

/// A dead CFG input still shows up while a live node uses it.
impl graph_algorithms::walk::GraphRef for GraphWalkSuccs<'_> {
    type NodeId = NodeId;

    fn successors(&self, node: NodeId, mut f: impl FnMut(NodeId)) {
        for value in self.0.node_inputs(node) {
            f(self.0.value_definition(value).0);
        }
        for succ in cfg_succs(self.0, node) {
            f(succ);
        }
    }
}

pub type GraphWalk<'a> = PreOrder<GraphWalkSuccs<'a>>;

/// Pre-order from `entry`, yielded first; reaches dead CFG inputs too.
pub(crate) fn walk_graph(graph: &Graph, entry: NodeId) -> GraphWalk<'_> {
    PreOrder::new(GraphWalkSuccs(graph), iter::once(entry))
}

/// Forward def-use successors within `live`.
pub(crate) fn live_def_use_succs<'a>(
    graph: &'a Graph,
    live: &'a NodeIdSet,
    node: NodeId,
) -> impl Iterator<Item = NodeId> + 'a {
    def_use_succs(graph, node).filter(|&succ| live.contains(succ))
}

/// A walk's reachable set plus its input-less `roots`.
#[derive(Debug, Clone)]
pub struct GraphWalkInfo {
    /// The reached nodes with no inputs.
    pub roots: Vec<NodeId>,
    pub live_nodes: DenseEntitySet<NodeId>,
}

impl GraphWalkInfo {
    /// Walks the mixed backward-input plus forward-control relation.
    pub fn compute_full(graph: &Graph, entry: NodeId) -> Self {
        let mut walk = walk_graph(graph, entry);
        let roots: Vec<NodeId> = walk
            .by_ref()
            .filter(|&n| graph.node_inputs(n).is_empty())
            .collect();

        Self {
            roots,
            live_nodes: walk.into_visited(),
        }
    }

    /// Every producer before its consumers unless the use closes a def-use
    /// cycle (a loop's `Region` or phi), a root first.
    pub(crate) fn reverse_postorder(&self, graph: &Graph) -> Vec<NodeId> {
        let succs = FnSuccs::new(|n| live_def_use_succs(graph, &self.live_nodes, n));
        let mut rpo: Vec<_> = PostOrder::new(succs, self.roots.iter().copied()).collect();
        rpo.reverse();
        rpo
    }
}

/// `InitialMemory`, every kind that consumes and re-produces a `Memory` token,
/// and `Load`, a leaf that only consumes one.
fn is_memory_chain_kind(kind: &NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::InitialMemory
            | NodeKind::Store(_)
            | NodeKind::Load(_)
            | NodeKind::Call { .. }
            | NodeKind::CallOther { .. }
            | NodeKind::MemPhi
    )
}

/// Consumers of `node`'s `Memory` output that are themselves chain kinds.
fn mem_succs(function: &Function, node: NodeId) -> impl Iterator<Item = NodeId> + '_ {
    function
        .memory_output_of(node)
        .ok()
        .into_iter()
        .flat_map(move |mem_value| function.value_uses(mem_value))
        .map(|(consumer, _slot)| consumer)
        .filter(|&consumer| is_memory_chain_kind(function.node_kind(consumer)))
}

/// The memory-chain nodes reachable forward from the entry-reachable
/// `InitialMemory`, in pre-order, root included; empty without one. Excludes
/// mere token readers (`Return`, `IndirectBranch`). Follows raw use-lists, so
/// on an uncompacted graph it can include a dead `Store` on the live token.
pub fn memory_reachable(function: &Function) -> Vec<NodeId> {
    let graph = function.graph();
    let Some(root) = walk_graph(graph, function.entry())
        .find(|&n| matches!(graph.node_kind(n), NodeKind::InitialMemory))
    else {
        return Vec::new();
    };

    PreOrder::new(FnSuccs::new(|n| mem_succs(function, n)), iter::once(root)).collect()
}
