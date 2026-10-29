use core::iter;

pub use entity_utils::set::DenseEntitySet;

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
    // The walk's visited set IS the answer, so the yielded items are dropped.
    let mut walk = PreOrder::new(CfgSuccs(graph), iter::once(entry));
    walk.by_ref().for_each(drop);
    walk.into_visited()
}

/// Control-reachable from `entry` and unable to reach a terminator: an
/// exit-free control cycle plus everything that only reaches it, which
/// `validate` rejects and the lifter seats an `Unreachable` sink on.
///
/// A dangling control output counts as an exit, so an already-malformed
/// function reports that malformation rather than this one.
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

    let mut stranded = NodeIdSet::new();
    for node in cfg.iter().filter(|&node| !escapes.contains(node)) {
        stranded.insert(node);
    }
    stranded
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

pub type PreOrder<G> = graph_algorithms::walk::PreOrder<G>;

pub type PostOrder<G> = graph_algorithms::walk::PostOrder<G>;

pub use graph_algorithms::walk::WalkPhase;

/// Successors follow data inputs backward and control edges forward.
#[derive(Clone, Copy)]
pub struct GraphWalkSuccs<'a>(&'a Graph);

impl<'a> GraphWalkSuccs<'a> {
    #[inline]
    pub(crate) fn new(graph: &'a Graph) -> Self {
        Self(graph)
    }
}

/// Two disjoint sets: every data predecessor (walking value / memory /
/// dispatch edges BACKWARD, so each def precedes its uses) and every CFG
/// successor (walking control edges FORWARD).
///
/// Dead CFG inputs still show up while they hang off a live node as data.
pub(crate) fn graph_walk_succs(graph: &Graph, node: NodeId) -> impl Iterator<Item = NodeId> + '_ {
    graph
        .node_inputs(node)
        .into_iter()
        .map(move |value| graph.value_definition(value).0)
        .chain(cfg_succs(graph, node))
}

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

/// Forward def-use successors, unrestricted by liveness.
fn def_use_succs(graph: &Graph, node: NodeId) -> impl Iterator<Item = NodeId> + '_ {
    graph
        .node_outputs(node)
        .iter()
        .flat_map(move |output| graph.value_uses(*output))
        .map(|(succ, _use_idx)| succ)
}

impl graph_algorithms::walk::GraphRef for GraphWalkSuccs<'_> {
    type NodeId = NodeId;

    fn successors(&self, node: NodeId, mut f: impl FnMut(NodeId)) {
        for succ in graph_walk_succs(self.0, node) {
            f(succ);
        }
    }
}

/// Forward control edges only.
#[derive(Clone, Copy)]
struct CfgSuccs<'a>(&'a Graph);

impl graph_algorithms::walk::GraphRef for CfgSuccs<'_> {
    type NodeId = NodeId;

    fn successors(&self, node: NodeId, mut f: impl FnMut(NodeId)) {
        for succ in cfg_succs(self.0, node) {
            f(succ);
        }
    }
}

pub type GraphWalk<'a> = PreOrder<GraphWalkSuccs<'a>>;

/// Pre-order from `entry`, so `entry` is yielded FIRST. Order is otherwise
/// unspecified, and "reachable" includes dead CFG inputs.
pub(crate) fn walk_graph(graph: &Graph, entry: NodeId) -> GraphWalk<'_> {
    PreOrder::new(GraphWalkSuccs::new(graph), iter::once(entry))
}

pub(crate) type DefUsePostorder<'a> = PostOrder<DefUseSuccs<'a>>;

/// The liveness-unrestricted counterpart of [`DefUseSuccs`]: a post-order
/// from some roots reaches every transitive consumer, dead ones included.
#[derive(Clone, Copy)]
pub(crate) struct RawDefUseSuccs<'a>(&'a Graph);

impl<'a> RawDefUseSuccs<'a> {
    #[inline]
    pub(crate) fn new(graph: &'a Graph) -> Self {
        Self(graph)
    }
}

impl graph_algorithms::walk::GraphRef for RawDefUseSuccs<'_> {
    type NodeId = NodeId;

    fn successors(&self, node: NodeId, mut f: impl FnMut(NodeId)) {
        for succ in def_use_succs(self.0, node) {
            f(succ);
        }
    }
}

/// Forward def-use edges restricted to a precomputed live set.
#[derive(Clone, Copy)]
pub(crate) struct DefUseSuccs<'a> {
    graph: &'a Graph,
    live_nodes: &'a DenseEntitySet<NodeId>,
}

impl<'a> DefUseSuccs<'a> {
    #[inline]
    pub(crate) fn new(graph: &'a Graph, live_nodes: &'a DenseEntitySet<NodeId>) -> Self {
        Self { graph, live_nodes }
    }
}

impl graph_algorithms::walk::GraphRef for DefUseSuccs<'_> {
    type NodeId = NodeId;

    fn successors(&self, node: NodeId, mut f: impl FnMut(NodeId)) {
        let live = def_use_succs(self.graph, node).filter(|&succ| self.live_nodes.contains(succ));
        for succ in live {
            f(succ);
        }
    }
}

/// A walk's reachable set plus its input-less `roots`.
#[derive(Debug, Clone)]
pub struct GraphWalkInfo {
    /// Input-less source nodes: `Entry`, constants, `InitialVar`,
    /// `InitialMemory`.
    pub roots: Vec<NodeId>,
    pub live_nodes: DenseEntitySet<NodeId>,
}

impl GraphWalkInfo {
    /// Walks the mixed backward-data plus forward-control relation.
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

    /// Every node is yielded after its consumers, unless the use closes a
    /// def-use cycle.
    pub(crate) fn postorder<'a>(&'a self, graph: &'a Graph) -> DefUsePostorder<'a> {
        PostOrder::new(
            DefUseSuccs::new(graph, &self.live_nodes),
            self.roots.iter().copied(),
        )
    }

    /// Every producer before its consumers unless the use closes a def-use
    /// cycle (a loop's `Region` or phi), a root first.
    pub(crate) fn reverse_postorder(&self, graph: &Graph) -> Vec<NodeId> {
        let mut rpo: Vec<_> = self.postorder(graph).collect();
        rpo.reverse();
        rpo
    }
}

/// The `InitialMemory` root plus every node that consumes and re-produces a
/// `Memory` token. `Load` is the exception: it consumes but produces none, so
/// it is always a leaf.
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

/// The forward memory-token chain.
#[derive(Clone, Copy)]
struct MemorySuccs<'a>(&'a Function);

impl graph_algorithms::walk::GraphRef for MemorySuccs<'_> {
    type NodeId = NodeId;

    fn successors(&self, node: NodeId, mut f: impl FnMut(NodeId)) {
        for succ in mem_succs(self.0, node) {
            f(succ);
        }
    }
}

/// The memory-touching nodes reachable forward from `function`'s
/// `InitialMemory` root, in pre-order, the root included.
///
/// Consumers that merely read the final token without touching memory (a
/// `Return`'s memory input, an `IndirectBranch`'s memory slot) are excluded.
/// Empty when no `InitialMemory` is reachable from the entry.
///
/// The walk follows structural use-lists, so on a NON-compacted graph the
/// result can include a memory op that is not itself reachable from it,
/// e.g. a dead `Store` still consuming the live token.
pub fn memory_reachable(function: &Function) -> Vec<NodeId> {
    let graph = function.graph();
    let live = GraphWalkInfo::compute_full(graph, function.entry()).live_nodes;
    let Some(root) = function
        .reachable_kind_iter(&live)
        .find(|(_, k)| matches!(k, NodeKind::InitialMemory))
        .map(|(n, _)| n)
    else {
        return Vec::new();
    };

    // The walk's visited tracker dedups and breaks cycles, such as a
    // loop-header `MemPhi` back-edge.
    PreOrder::new(MemorySuccs(function), iter::once(root)).collect()
}
