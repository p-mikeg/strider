use core::iter;
use core::ops::ControlFlow;

pub use entity_utils::set::DenseEntitySet;

use crate::IRViewer;
use crate::function::Function;
use crate::graph::Graph;
use crate::node::{NodeId, NodeKind, ValueId};

mod cast;
pub use cast::{CastMask, cast_mask_of};

/// Convenience alias for the `cfg_reachable` return shape.  Re-exported so
/// downstream crates can take `&NodeIdSet` parameters without depending on
/// `entity_utils` directly.
pub type NodeIdSet = DenseEntitySet<NodeId>;

/// Returns the set of all nodes reachable from `entry` following only
/// `Control`-kind edges — the CFG skeleton.
///
/// Data edges (value, memory, PhiToken) are not followed, so only
/// control-flow nodes (`Entry`, `Region`, `If`, `Return`, `Call`, …)
/// appear in the result.
///
/// This is used by optimisation passes (e.g. `PhiCollapse`) to determine
/// which basic-block headers are live and which predecessor slots on `Region`,
/// `Phi`, and `MemPhi` nodes are dead.
pub fn cfg_reachable(graph: &Graph, entry: NodeId) -> DenseEntitySet<NodeId> {
    // A pre-order walk over the control-only successor relation ([`CfgSuccs`])
    // visits exactly the control-reachable nodes (the generic `PreOrder`'s
    // `DenseEntitySet` tracker is the same insert-as-dedup gate the old
    // hand-rolled worklist used); its visited set IS the result.
    let mut walk = PreOrder::new(CfgSuccs(graph), iter::once(entry));
    walk.by_ref().for_each(drop);
    walk.into_visited()
}

/// A pre-order walk over the IR graph using a [`DenseEntitySet`] as the
/// visited tracker.
pub type PreOrder<G> = graph_algorithms::walk::PreOrder<G, DenseEntitySet<NodeId>>;

/// A post-order walk over the IR graph using a [`DenseEntitySet`] as the
/// visited tracker.
pub type PostOrder<G> = graph_algorithms::walk::PostOrder<G, DenseEntitySet<NodeId>>;

/// A [`graph_algorithms::walk::GraphRef`] implementation that drives successor enumeration
/// for IR graph walks.
///
/// Successors are derived by following both data inputs (so every producer is
/// visited before a consumer in a reverse-topological traversal) and outgoing
/// control edges.
#[derive(Clone, Copy)]
pub struct GraphWalkSuccs<'a>(&'a Graph);

impl<'a> GraphWalkSuccs<'a> {
    /// Wraps `graph` in a `GraphWalkSuccs` adaptor.
    #[inline]
    pub(crate) fn new(graph: &'a Graph) -> Self {
        Self(graph)
    }
}

/// Returns the combined "successor" set used by the general graph walk.
///
/// Yields two disjoint sets of nodes:
/// 1. Every **data predecessor** (producer of each of `node`'s inputs) — walks
///    backward through value, memory, and dispatch edges so that every def is
///    visited before any use in the resulting traversal.
/// 2. Every **CFG successor** (consumer of each of `node`'s `Control` outputs)
///    — walks forward so the whole reachable control graph is covered.
///
/// The mix of backward-data and forward-control is intentional: it ensures
/// that neither producers nor consumers are missed in a single pass starting
/// from the function entry node.  Dead CFG inputs (nodes whose control
/// predecessor was eliminated) are still visited if they remain attached as
/// data inputs to live nodes; callers that need to distinguish live from dead
/// nodes should consult [`cfg_reachable`].
pub(crate) fn graph_walk_succs(graph: &Graph, node: NodeId) -> impl Iterator<Item = NodeId> + '_ {
    graph
        .node_inputs(node)
        .into_iter()
        .map(move |value| graph.value_definition(value).0)
        .chain(cfg_succs(graph, node))
}

/// Returns an iterator over all `Control`-kind outputs of `node`.
pub(crate) fn cfg_outputs(graph: &Graph, node: NodeId) -> impl Iterator<Item = ValueId> + '_ {
    graph
        .node_outputs(node)
        .iter()
        .copied()
        .filter(|&output| graph.value_kind(output).is_control())
}

/// Returns an iterator over all nodes that consume a `Control` output of `node`.
pub(crate) fn cfg_succs(graph: &Graph, node: NodeId) -> impl Iterator<Item = NodeId> + '_ {
    cfg_outputs(graph, node)
        .flat_map(|output| graph.value_uses(output))
        .map(|(succ_node, _succ_input_idx)| succ_node)
}

/// Forward def→use successors of `node`: every node consuming one of `node`'s
/// outputs, unrestricted by liveness.  Shared by [`RawDefUseSuccs`] (directly)
/// and [`DefUseSuccs`] (live-filtered).
fn def_use_succs(graph: &Graph, node: NodeId) -> impl Iterator<Item = NodeId> + '_ {
    graph
        .node_outputs(node)
        .iter()
        .flat_map(move |output| graph.value_uses(*output))
        .map(|(succ, _use_idx)| succ)
}

impl graph_algorithms::walk::GraphRef for GraphWalkSuccs<'_> {
    type NodeId = NodeId;

    fn try_successors(
        &self,
        node: NodeId,
        f: impl FnMut(NodeId) -> ControlFlow<()>,
    ) -> ControlFlow<()> {
        graph_walk_succs(self.0, node).try_for_each(f)
    }
}

/// A [`graph_algorithms::walk::GraphRef`] over forward **control** edges only
/// (via [`cfg_succs`]) — the successor relation [`cfg_reachable`] walks to find
/// the control-live node set.
#[derive(Clone, Copy)]
struct CfgSuccs<'a>(&'a Graph);

impl graph_algorithms::walk::GraphRef for CfgSuccs<'_> {
    type NodeId = NodeId;

    fn try_successors(
        &self,
        node: NodeId,
        f: impl FnMut(NodeId) -> ControlFlow<()>,
    ) -> ControlFlow<()> {
        cfg_succs(self.0, node).try_for_each(f)
    }
}

/// The concrete pre-order walk type used by [`crate::IRWalker::walk`].
pub type GraphWalk<'a> = PreOrder<GraphWalkSuccs<'a>>;

/// Walks all nodes reachable in `graph` from `entry` in an unspecified order.
///
/// Note that "reachable" nodes here include dead CFG inputs.
///
/// `entry` is guaranteed to be the last node returned if it has no inputs (as should be the case
/// with every well-formed graph).
///
/// Crate-private: external callers must route through [`crate::IRWalker::walk`]
/// so the `Graph` methods stay the single public entry-point surface.
pub(crate) fn walk_graph(graph: &Graph, entry: NodeId) -> GraphWalk<'_> {
    PreOrder::new(GraphWalkSuccs::new(graph), iter::once(entry))
}

/// The forward def→use post-order walk backing the real reverse-post-order
/// ([`GraphWalkInfo::reverse_postorder`]).
pub type DefUsePostorder<'a> = PostOrder<DefUseSuccs<'a>>;

/// A [`graph_algorithms::walk::GraphRef`] over the forward def→use edges, **unrestricted**
/// by liveness — the raw counterpart of [`DefUseSuccs`].
///
/// Driving a post-order with this relation from a set of roots visits every
/// transitive consumer of those roots, including dead ones (consumers no
/// longer reachable from the function entry).  The self-cleaning rewrite
/// context's initial cull needs exactly this: it must reach the pre-existing
/// dead consumers of still-live producers so their stale input edges can be
/// detached.
#[derive(Clone, Copy)]
pub struct RawDefUseSuccs<'a>(&'a Graph);

impl<'a> RawDefUseSuccs<'a> {
    /// Wraps `graph` in an unrestricted forward def→use successor adaptor.
    #[inline]
    pub fn new(graph: &'a Graph) -> Self {
        Self(graph)
    }
}

impl graph_algorithms::walk::GraphRef for RawDefUseSuccs<'_> {
    type NodeId = NodeId;

    fn try_successors(
        &self,
        node: NodeId,
        f: impl FnMut(NodeId) -> ControlFlow<()>,
    ) -> ControlFlow<()> {
        def_use_succs(self.0, node).try_for_each(f)
    }
}

/// A [`graph_algorithms::walk::GraphRef`] over the forward def→use edges, restricted to a
/// precomputed live set. Driving a post-order with it yields every node
/// after all of its uses, so reversing the post-order gives a true RPO
/// (every producer strictly before its consumers).
#[derive(Clone, Copy)]
pub struct DefUseSuccs<'a> {
    graph: &'a Graph,
    live_nodes: &'a DenseEntitySet<NodeId>,
}

impl<'a> DefUseSuccs<'a> {
    /// Wraps `graph` and the reachable `live_nodes` set in a successor adaptor.
    #[inline]
    pub fn new(graph: &'a Graph, live_nodes: &'a DenseEntitySet<NodeId>) -> Self {
        Self { graph, live_nodes }
    }
}

impl graph_algorithms::walk::GraphRef for DefUseSuccs<'_> {
    type NodeId = NodeId;

    fn try_successors(
        &self,
        node: NodeId,
        f: impl FnMut(NodeId) -> ControlFlow<()>,
    ) -> ControlFlow<()> {
        def_use_succs(self.graph, node)
            .filter(|&succ| self.live_nodes.contains(succ))
            .try_for_each(f)
    }
}

/// The reachable set of a graph walk plus its source `roots`, the inputs to
/// a real reverse-post-order.
///
/// [`compute_full`](Self::compute_full) discovers the entry-reachable nodes
/// (via the mixed `GraphWalkSuccs` relation, identical to `walk_graph`)
/// and records the input-less `roots` (`Entry` / constants / `InitialVar` /
/// `InitialMemory`). [`reverse_postorder`](Self::reverse_postorder) then
/// post-orders the forward def→use graph from those roots and reverses it,
/// so every producer precedes its consumers — a genuine RPO, unlike a
/// post-order over the mixed (part-backward) successor relation.
#[derive(Debug, Clone)]
pub struct GraphWalkInfo {
    /// Input-less source nodes reachable from the walk entry.
    pub roots: Vec<NodeId>,
    /// Every node reachable from the walk entry.
    pub live_nodes: DenseEntitySet<NodeId>,
}

impl GraphWalkInfo {
    /// Walk `graph` from `entry` (mixed backward-data + forward-control),
    /// recording the reachable `live_nodes` and the input-less `roots`.
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

    /// Post-order over the forward def→use graph from `roots`, restricted to
    /// `live_nodes`: every node is yielded after all of its consumers.
    pub fn postorder<'a>(&'a self, graph: &'a Graph) -> DefUsePostorder<'a> {
        PostOrder::new(
            DefUseSuccs::new(graph, &self.live_nodes),
            self.roots.iter().copied(),
        )
    }

    /// Reverse-post-order (real RPO): the reverse of [`postorder`](Self::postorder),
    /// so every producer is yielded strictly before its consumers, roots first.
    pub fn reverse_postorder(&self, graph: &Graph) -> Vec<NodeId> {
        let mut rpo: Vec<_> = self.postorder(graph).collect();
        rpo.reverse();
        rpo
    }
}

/// Whether `kind` is one of the memory-chain node kinds `memory_reachable`
/// walks and reports: the `InitialMemory` root plus every node that both
/// consumes and re-produces a `Memory` token (`Load` is the one exception —
/// it consumes but produces none, so it is always a leaf of the walk).
fn is_memory_chain_kind(kind: &NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::InitialMemory
            | NodeKind::Store(_)
            | NodeKind::Load(_)
            | NodeKind::Call
            | NodeKind::CallOther { .. }
            | NodeKind::MemPhi
    )
}

/// Returns `node`'s memory-chain successors: the consumers of its `Memory`
/// output (if it has one) that are themselves memory-chain kinds
/// (`is_memory_chain_kind`). `Load` has no `Memory` output, so it is
/// naturally a leaf. Non-chain consumers of a `Memory` value (e.g. a
/// `Return`'s memory input) are filtered out here, at the successor
/// relation, rather than by the caller.
fn mem_succs(function: &Function, node: NodeId) -> impl Iterator<Item = NodeId> + '_ {
    function
        .memory_output_of(node)
        .ok()
        .into_iter()
        .flat_map(move |mem_value| function.value_uses(mem_value))
        .map(|(consumer, _slot)| consumer)
        .filter(|&consumer| is_memory_chain_kind(function.node_kind(consumer)))
}

/// A [`graph_algorithms::walk::GraphRef`] over the forward memory-token chain
/// ([`mem_succs`]) — the successor relation [`memory_reachable`] walks from
/// the function's `InitialMemory` root.
#[derive(Clone, Copy)]
struct MemorySuccs<'a>(&'a Function);

impl graph_algorithms::walk::GraphRef for MemorySuccs<'_> {
    type NodeId = NodeId;

    fn try_successors(
        &self,
        node: NodeId,
        f: impl FnMut(NodeId) -> ControlFlow<()>,
    ) -> ControlFlow<()> {
        mem_succs(self.0, node).try_for_each(f)
    }
}

/// The memory-touching nodes reachable by following memory-token edges
/// forward from `function`'s `InitialMemory` root, in pre-order.
///
/// The memory chain is rooted at the unique `InitialMemory` node; its
/// `Memory` output feeds the next memory op (`Load` / `Store` / `Call` /
/// `CallOther` / `MemPhi`), and each producer's own `Memory` output
/// continues the chain. `Load` is a leaf — it consumes a memory token but
/// produces none. The `InitialMemory` root is included.
///
/// A `Memory`-typed value's use-list can include a non-chain consumer that
/// merely reads the final token without itself touching memory (a `Return`'s
/// or `Unreachable`'s memory input, an `IndirectBranch`'s memory slot, …);
/// `is_memory_chain_kind` excludes those from both the output and any
/// further traversal (they produce no `Memory` output of their own, so
/// excluding them from the walk changes nothing structurally — only the
/// reported node set).
///
/// `InitialMemory` is a data root with no control edges of its own, so it is
/// located via the same mixed backward-data + forward-control reachable set
/// [`crate::validate::validate`] uses ([`GraphWalkInfo::compute_full`]), not
/// the control-only [`cfg_reachable`] skeleton (which would never see it).
///
/// **Complexity: O(V+E) over the whole function, not O(memory nodes +
/// edges).** Locating the root requires a full data-inclusive reachability
/// pass — `InitialMemory` is a data root, not control-reachable, so the
/// cheap `cfg_reachable` walk can't find it — before the memory-chain walk
/// even starts. That walk itself is only O(memory nodes + edges), but it's
/// dominated by the O(V+E) `compute_full` prefix. This runs once per call,
/// which is acceptable for a user-facing query — [`crate::validate::validate`]
/// and `data_walk` pay the identical O(V+E) cost. A future O(memory-chain)
/// version would need `Function` to cache the `InitialMemory` `NodeId`
/// directly (the way it already caches `entry`), skipping the root-finding
/// walk entirely; that's a deferred follow-up, out of scope here.
///
/// Returns an empty `Vec` only when no `InitialMemory` node is reachable from
/// `entry` — a partial or unterminated graph where nothing consumes the
/// initial memory token. A validated function normally keeps `InitialMemory`
/// live through its `Return`'s memory input, so even a function with no
/// `Load` / `Store` / `Call` still returns `InitialMemory` (plus any entry
/// `MemPhi`).
///
/// The forward walk follows structural memory use-lists, so on a
/// non-compacted graph the result can include a memory op that is not itself
/// reachable from `entry` (a dead `Store` still consumes the live token). On
/// the normal compacted analysis path such nodes are already gone, so the
/// result is the entry-reachable memory chain.
pub fn memory_reachable(function: &Function, entry: NodeId) -> Vec<NodeId> {
    let graph = function.graph();
    let live = GraphWalkInfo::compute_full(graph, entry).live_nodes;
    let Some(root) = function
        .reachable_kind_iter(&live)
        .find(|(_, k)| matches!(k, NodeKind::InitialMemory))
        .map(|(n, _)| n)
    else {
        return Vec::new();
    };

    // A pre-order walk over the memory-chain successor relation
    // ([`MemorySuccs`]) visits exactly the memory-chain-reachable nodes; the
    // generic `PreOrder`'s `DenseEntitySet` tracker dedups and breaks cycles
    // (e.g. a loop-header `MemPhi` back-edge) the same way the old
    // hand-rolled worklist's `seen` set did.
    PreOrder::new(MemorySuccs(function), iter::once(root)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::{NodeKind, ValueKind, ValueType};
    use cranelift_entity::EntityRef;

    // ── helpers ───────────────────────────────────────────────────────────────

    /// Creates an Entry node with a single Control output.  Returns the node
    /// id and the control output id.
    fn make_entry(graph: &mut Graph) -> (NodeId, ValueId) {
        let entry = graph.create_node(NodeKind::Entry, [], [ValueKind::Control]);
        let [ctrl] = graph.node_outputs_exact::<1>(entry).unwrap();
        (entry, ctrl)
    }

    /// Creates a non-cacheable Region node that produces one Control
    /// output, and wires `ctrl_value` as its first input so that the producer
    /// of `ctrl_value` has this node as a CFG successor.
    fn make_ctrl_node(graph: &mut Graph, ctrl_value: ValueId) -> (NodeId, ValueId) {
        let node = graph.create_node(NodeKind::Region, [], [ValueKind::Control]);
        graph.add_node_input(node, ctrl_value);
        let [value] = graph.node_outputs_exact::<1>(node).unwrap();
        (node, value)
    }

    /// Creates a Return node (leaf, non-cacheable) that consumes `ctrl_value`
    /// as its only input, making the producer of `ctrl_value` a CFG predecessor.
    fn make_return(graph: &mut Graph, ctrl_value: ValueId) -> NodeId {
        let node = graph.create_node(NodeKind::Return, [], []);
        graph.add_node_input(node, ctrl_value);
        node
    }

    // ── walk_graph ────────────────────────────────────────────────────────────

    /// An entry node with no successors must be visited exactly once.
    #[test]
    fn walk_single_entry_visits_exactly_one_node() {
        let mut graph = Graph::new();
        let (entry, _ctrl) = make_entry(&mut graph);
        let visited: Vec<_> = walk_graph(&graph, entry).collect();
        assert_eq!(visited, vec![entry]);
    }

    /// A linear chain entry → A → B must be fully traversed: all three nodes
    /// must appear exactly once.
    #[test]
    fn walk_linear_chain_visits_all_nodes() {
        let mut graph = Graph::new();
        let (entry, entry_ctrl) = make_entry(&mut graph);
        let (a, a_ctrl) = make_ctrl_node(&mut graph, entry_ctrl);
        let b = make_return(&mut graph, a_ctrl);

        let visited: Vec<_> = walk_graph(&graph, entry).collect();
        assert_eq!(visited.len(), 3, "all three nodes must be visited");
        assert!(visited.contains(&entry));
        assert!(visited.contains(&a));
        assert!(visited.contains(&b));
    }

    /// A longer chain entry → A → B → C → D must be fully traversed.
    #[test]
    fn walk_long_chain_visits_all_nodes() {
        let mut graph = Graph::new();
        let (entry, c0) = make_entry(&mut graph);
        let (a, c1) = make_ctrl_node(&mut graph, c0);
        let (b, c2) = make_ctrl_node(&mut graph, c1);
        let (c, c3) = make_ctrl_node(&mut graph, c2);
        let d = make_return(&mut graph, c3);

        let visited: Vec<_> = walk_graph(&graph, entry).collect();
        assert_eq!(visited.len(), 5);
        for node in [entry, a, b, c, d] {
            assert!(visited.contains(&node), "{node:?} missing from walk");
        }
    }

    /// A diamond-shaped graph (entry → left, right → merge) must visit each
    /// node exactly once despite the converging control edges.
    #[test]
    fn walk_diamond_visits_each_node_once() {
        let mut graph = Graph::new();

        // Entry with two control outputs: one for left, one for right.
        let entry = graph.create_node(
            NodeKind::Entry,
            [],
            [ValueKind::Control, ValueKind::Control],
        );
        let [ctrl_l, ctrl_r] = graph.node_outputs_exact::<2>(entry).unwrap();

        let (_left, left_ctrl) = make_ctrl_node(&mut graph, ctrl_l);
        let (_right, right_ctrl) = make_ctrl_node(&mut graph, ctrl_r);

        // Merge consumes both branch ctrl outputs.
        let merge = graph.create_node(NodeKind::Return, [], []);
        graph.add_node_input(merge, left_ctrl);
        graph.add_node_input(merge, right_ctrl);

        let visited: Vec<_> = walk_graph(&graph, entry).collect();
        assert_eq!(visited.len(), 4, "diamond must produce exactly 4 nodes");

        // No duplicates.
        let mut seen = std::collections::HashSet::new();
        for n in &visited {
            assert!(seen.insert(*n), "node {n:?} was visited more than once");
        }
    }

    /// Nodes that are not reachable from the entry (no control or data path)
    /// must not appear in the walk output.
    #[test]
    fn walk_does_not_visit_unreachable_nodes() {
        let mut graph = Graph::new();
        let (entry, _ctrl) = make_entry(&mut graph);
        // Isolated node: completely disconnected.
        let isolated = graph.create_node(NodeKind::Return, [], []);

        let visited: Vec<_> = walk_graph(&graph, entry).collect();
        assert!(
            !visited.contains(&isolated),
            "isolated node must not be visited"
        );
        assert!(visited.contains(&entry));
    }

    /// A data-only fan-out (one value consumed by multiple sinks) must
    /// visit the source and all sinks.
    #[test]
    fn walk_follows_data_inputs_to_producer() {
        let mut graph = Graph::new();
        // A pure data source (no control outputs).
        let src = graph.create_node(
            NodeKind::IntConst(crate::node::const_value::ConstId::new(42_usize)),
            [],
            [ValueKind::Typed(ValueType::I64)],
        );
        let [data_value] = graph.node_outputs_exact::<1>(src).unwrap();

        // Entry → sink1 and sink2, both also consuming the data value.
        let (entry, entry_ctrl) = make_entry(&mut graph);
        let sink1 = graph.create_node(NodeKind::Return, [], []);
        graph.add_node_input(sink1, entry_ctrl);
        graph.add_node_input(sink1, data_value);

        let sink2 = graph.create_node(NodeKind::Return, [], []);
        // sink2 is only reachable via data input from sink1's producer (entry_ctrl consumed by sink1, not sink2)
        // Actually attach sink2 to data_value only - it won't be reachable from entry via control
        // but via data: walk from entry visits sink1 (cfg succ), sink1's inputs point to entry and src,
        // src has no inputs, so src is visited. sink2 is not reachable at all.
        graph.add_node_input(sink2, data_value);

        let visited: Vec<_> = walk_graph(&graph, entry).collect();
        // entry → sink1 (cfg succ), sink1's inputs → entry (visited), src (not visited yet)
        // src has no inputs or cfg succs.
        // sink2 is reachable only as a consumer of data_value (via value_uses), but
        // graph_walk_succs does NOT follow output uses — it follows inputs.
        assert!(visited.contains(&entry));
        assert!(visited.contains(&sink1));
        assert!(
            visited.contains(&src),
            "src is reachable via sink1's data input"
        );
        assert!(
            !visited.contains(&sink2),
            "sink2 has no path from entry through inputs"
        );
    }

    // ── cfg_succs ─────────────────────────────────────────────────────────────

    /// A node with no Control outputs must have no CFG successors.
    #[test]
    fn cfg_succs_no_control_outputs_is_empty() {
        let mut graph = Graph::new();
        let node = graph.create_node(
            NodeKind::IntConst(crate::node::const_value::ConstId::new(0_usize)),
            [],
            [ValueKind::Typed(ValueType::I64)],
        );
        let succs: Vec<_> = cfg_succs(&graph, node).collect();
        assert!(
            succs.is_empty(),
            "data-only node must have no cfg successors"
        );
    }

    /// A node whose single Control output is consumed by two different nodes
    /// must appear as a predecessor of both.
    #[test]
    fn cfg_succs_returns_all_control_consumers() {
        let mut graph = Graph::new();
        let (entry, ctrl) = make_entry(&mut graph);

        let r0 = make_return(&mut graph, ctrl);
        let r1 = make_return(&mut graph, ctrl);

        let succs: Vec<_> = cfg_succs(&graph, entry).collect();
        assert_eq!(succs.len(), 2, "both consumers must appear");
        assert!(succs.contains(&r0));
        assert!(succs.contains(&r1));
    }

    /// A node with two Control outputs leading to different successors must
    /// report both successors.
    #[test]
    fn cfg_succs_two_control_outputs_two_successors() {
        let mut graph = Graph::new();
        let entry = graph.create_node(
            NodeKind::Entry,
            [],
            [ValueKind::Control, ValueKind::Control],
        );
        let [ctrl0, ctrl1] = graph.node_outputs_exact::<2>(entry).unwrap();

        let left = make_return(&mut graph, ctrl0);
        let right = make_return(&mut graph, ctrl1);

        let succs: Vec<_> = cfg_succs(&graph, entry).collect();
        assert_eq!(succs.len(), 2);
        assert!(succs.contains(&left));
        assert!(succs.contains(&right));
    }

    /// A node with a Control output that has no consumers must not report any
    /// CFG successors for that output.
    #[test]
    fn cfg_succs_unconsumed_control_output_yields_nothing() {
        let mut graph = Graph::new();
        let (entry, _ctrl) = make_entry(&mut graph);
        // ctrl is produced but never consumed.
        let succs: Vec<_> = cfg_succs(&graph, entry).collect();
        assert!(succs.is_empty());
    }

    // ── cfg_outputs ───────────────────────────────────────────────────────────

    /// `cfg_outputs` must only return outputs with `ValueKind::Control`.
    /// Data and memory outputs must be excluded.
    #[test]
    fn cfg_outputs_excludes_non_control_outputs() {
        let mut graph = Graph::new();
        // Region is non-cacheable so we can give it arbitrary outputs.
        let node = graph.create_node(
            NodeKind::Region,
            [],
            [
                ValueKind::Control,
                ValueKind::Typed(ValueType::I64),
                ValueKind::Memory,
                ValueKind::Control,
            ],
        );
        let ctrl_outs: Vec<_> = cfg_outputs(&graph, node).collect();
        assert_eq!(
            ctrl_outs.len(),
            2,
            "only the two Control outputs must appear"
        );
        for value in ctrl_outs {
            assert_eq!(
                graph.value_kind(value),
                ValueKind::Control,
                "cfg_outputs must only yield Control-kind outputs"
            );
        }
    }

    /// A node with no outputs at all must yield an empty iterator from
    /// `cfg_outputs`.
    #[test]
    fn cfg_outputs_empty_for_node_with_no_outputs() {
        let mut graph = Graph::new();
        let node = graph.create_node(NodeKind::Return, [], []);
        let outs: Vec<_> = cfg_outputs(&graph, node).collect();
        assert!(outs.is_empty());
    }

    /// A node whose outputs are all non-control must yield nothing from
    /// `cfg_outputs`.
    #[test]
    fn cfg_outputs_empty_when_all_outputs_are_data() {
        let mut graph = Graph::new();
        let node = graph.create_node(
            NodeKind::IntConst(crate::node::const_value::ConstId::new(5_usize)),
            [],
            [ValueKind::Typed(ValueType::I32)],
        );
        let outs: Vec<_> = cfg_outputs(&graph, node).collect();
        assert!(outs.is_empty());
    }

    // ── rpo (defs-before-uses data-cone walk) ─────────────────────────────────

    /// `rpo` over `Add(InitialVar, IntConst)` must emit BOTH operands before
    /// the `Add` that consumes them (defs-before-uses). The seed node is last.
    #[test]
    fn rpo_emits_operands_before_consumer() {
        let mut graph = Graph::new();
        let a = graph.create_node(
            NodeKind::IntConst(crate::node::const_value::ConstId::new(5_usize)),
            [],
            [ValueKind::Typed(ValueType::I64)],
        );
        let [a_value] = graph.node_outputs_exact::<1>(a).unwrap();
        let c = graph.create_node(
            NodeKind::IntConst(crate::node::const_value::ConstId::new(4_usize)),
            [],
            [ValueKind::Typed(ValueType::I64)],
        );
        let [c_value] = graph.node_outputs_exact::<1>(c).unwrap();
        let add = graph.create_node(
            NodeKind::IntBinaryOp(crate::IntBinaryOp::Add),
            [a_value, c_value],
            [ValueKind::Typed(ValueType::I64)],
        );
        let [_add_value] = graph.node_outputs_exact::<1>(add).unwrap();

        let order: Vec<NodeId> =
            crate::walk::GraphWalkInfo::compute_full(&graph, add).reverse_postorder(&graph);

        assert_eq!(
            order.len(),
            3,
            "rpo must visit each cone node once: {order:?}"
        );
        let pos = |n: NodeId| order.iter().position(|&x| x == n).unwrap();
        assert!(pos(a) < pos(add), "first IntConst must precede Add");
        assert!(pos(c) < pos(add), "second IntConst must precede Add");
        assert_eq!(order[2], add, "seed (Add) is emitted last");
    }

    /// `rpo` follows only data inputs and visits a shared operand once.
    #[test]
    fn rpo_visits_shared_operand_once() {
        let mut graph = Graph::new();
        let c = graph.create_node(
            NodeKind::IntConst(crate::node::const_value::ConstId::new(7_usize)),
            [],
            [ValueKind::Typed(ValueType::I64)],
        );
        let [c_value] = graph.node_outputs_exact::<1>(c).unwrap();
        let add = graph.create_node(
            NodeKind::IntBinaryOp(crate::IntBinaryOp::Add),
            [c_value, c_value],
            [ValueKind::Typed(ValueType::I64)],
        );
        let [_add_value] = graph.node_outputs_exact::<1>(add).unwrap();

        let order: Vec<NodeId> =
            crate::walk::GraphWalkInfo::compute_full(&graph, add).reverse_postorder(&graph);
        assert_eq!(
            order,
            vec![c, add],
            "shared operand visited once, before Add"
        );
    }

    // ── GraphWalkInfo / real RPO machinery ────────────────────────────────────

    /// Builds an `IntConst` and returns `(node, value)`.
    fn int_const(graph: &mut Graph, v: u64) -> (NodeId, ValueId) {
        let n = graph.create_node(
            NodeKind::IntConst(crate::node::const_value::ConstId::new((v) as usize)),
            [],
            [ValueKind::Typed(ValueType::I64)],
        );
        let [out] = graph.node_outputs_exact::<1>(n).unwrap();
        (n, out)
    }

    /// Builds an `IntBinaryOp(op)` over `[l, r]` and returns `(node, value)`.
    fn int_bin(
        graph: &mut Graph,
        op: crate::IntBinaryOp,
        l: ValueId,
        r: ValueId,
    ) -> (NodeId, ValueId) {
        let n = graph.create_node(
            NodeKind::IntBinaryOp(op),
            [l, r],
            [ValueKind::Typed(ValueType::I64)],
        );
        let [out] = graph.node_outputs_exact::<1>(n).unwrap();
        (n, out)
    }

    /// `compute_full` records exactly the input-less nodes as `roots` and
    /// every reachable node in `live_nodes`.
    #[test]
    fn compute_full_records_roots_and_live_set() {
        let mut graph = Graph::new();
        let (k, kv) = int_const(&mut graph, 9);
        let neg = graph.create_node(
            NodeKind::IntUnaryOp(crate::IntUnaryOp::Neg),
            [kv],
            [ValueKind::Typed(ValueType::I64)],
        );
        let [negv] = graph.node_outputs_exact::<1>(neg).unwrap();
        let (add, _addv) = int_bin(&mut graph, crate::IntBinaryOp::Add, kv, negv);

        let info = GraphWalkInfo::compute_full(&graph, add);
        assert_eq!(
            info.roots,
            vec![k],
            "only the input-less IntConst is a root"
        );
        for n in [k, neg, add] {
            assert!(info.live_nodes.contains(n), "{n:?} must be live");
        }
    }

    /// A diamond — two consts feeding two ops that both feed a sink — must
    /// come out in strict defs-before-uses order: every operand strictly
    /// precedes each op that consumes it, along EVERY path.  This is the
    /// property a real RPO (post-order over the forward def→use graph,
    /// reversed) guarantees but a post-order over a part-backward relation
    /// does not.
    #[test]
    fn rpo_is_strict_defs_before_uses_on_a_diamond() {
        let mut graph = Graph::new();
        let (k1, k1v) = int_const(&mut graph, 1);
        let (k2, k2v) = int_const(&mut graph, 2);
        let (left, lv) = int_bin(&mut graph, crate::IntBinaryOp::Add, k1v, k2v);
        let (right, rv) = int_bin(&mut graph, crate::IntBinaryOp::Mul, k1v, k2v);
        let (sink, _sv) = int_bin(&mut graph, crate::IntBinaryOp::Add, lv, rv);

        let order =
            crate::walk::GraphWalkInfo::compute_full(&graph, sink).reverse_postorder(&graph);
        assert_eq!(order.len(), 5, "each node once: {order:?}");
        let pos = |n: NodeId| order.iter().position(|&x| x == n).unwrap();
        for op in [left, right] {
            assert!(pos(k1) < pos(op), "k1 before {op:?}: {order:?}");
            assert!(pos(k2) < pos(op), "k2 before {op:?}: {order:?}");
            assert!(pos(op) < pos(sink), "{op:?} before sink: {order:?}");
        }
        assert_eq!(*order.last().unwrap(), sink, "sink (sole consumer) is last");
    }

    /// A cycle (a back-edge feeding an earlier node) must terminate and visit
    /// each reachable node exactly once, roots first.  Built with the
    /// non-cacheable `Region` nodes (cacheable data nodes reject post-hoc
    /// input edits), matching a real loop-carried control back-edge.
    #[test]
    fn rpo_terminates_and_dedups_on_a_cycle() {
        use std::collections::HashSet;
        let mut graph = Graph::new();
        let (entry, e_ctrl) = make_entry(&mut graph);
        let (a, a_ctrl) = make_ctrl_node(&mut graph, e_ctrl);
        let (b, b_ctrl) = make_ctrl_node(&mut graph, a_ctrl);
        // Back-edge: A also consumes B's control → cycle A → B → A.
        graph.add_node_input(a, b_ctrl);

        let order =
            crate::walk::GraphWalkInfo::compute_full(&graph, entry).reverse_postorder(&graph);
        let unique: HashSet<NodeId> = order.iter().copied().collect();
        assert_eq!(
            order.len(),
            unique.len(),
            "no node visited twice despite the cycle: {order:?}"
        );
        for n in [entry, a, b] {
            assert!(
                unique.contains(&n),
                "{n:?} missing despite the cycle: {order:?}"
            );
        }
        assert_eq!(
            order.first(),
            Some(&entry),
            "input-less root (entry) first: {order:?}"
        );
    }

    // ── RawDefUseSuccs (unfiltered forward def→use) ───────────────────────────

    /// A raw def→use post-order from an input-less root reaches a consumer
    /// that is NOT in the live set — the case the filtered [`DefUseSuccs`]
    /// would skip but the initial cull needs.
    #[test]
    fn raw_def_use_postorder_reaches_dead_consumer() {
        let mut graph = Graph::new();
        // const → Neg(const).  `Neg` is the "dead" consumer: it's reachable
        // from the const via def→use, but we don't put it in any live set.
        let (k, kv) = int_const(&mut graph, 3);
        let neg = graph.create_node(
            NodeKind::IntUnaryOp(crate::IntUnaryOp::Neg),
            [kv],
            [ValueKind::Typed(ValueType::I64)],
        );

        // Filtered walk with an empty live set never leaves the root.
        let empty: DenseEntitySet<NodeId> = DenseEntitySet::new();
        let filtered: Vec<NodeId> =
            PostOrder::new(DefUseSuccs::new(&graph, &empty), std::iter::once(k)).collect();
        assert_eq!(filtered, vec![k], "filtered walk stays at the root");

        // Raw walk reaches the dead consumer.
        let raw: Vec<NodeId> =
            PostOrder::new(RawDefUseSuccs::new(&graph), std::iter::once(k)).collect();
        assert!(
            raw.contains(&neg),
            "raw walk must reach the dead consumer: {raw:?}"
        );
        assert!(raw.contains(&k), "raw walk includes the root: {raw:?}");
        // Post-order: consumer before producer.
        let pos = |n: NodeId| raw.iter().position(|&x| x == n).unwrap();
        assert!(
            pos(neg) < pos(k),
            "post-order yields the consumer before the producer"
        );
    }

    // ── reverse_postorder (global reverse-post-order) ─────────────────────────

    /// Global RPO over a linear chain entry → A → B (plus an unconnected
    /// data const consumed by the Return) must put `entry` FIRST and visit
    /// each reachable node exactly once.
    #[test]
    fn rpo_entry_first_visits_each_once() {
        use std::collections::HashSet;
        let mut graph = Graph::new();
        let (entry, c0) = make_entry(&mut graph);
        let (a, c1) = make_ctrl_node(&mut graph, c0);
        // Data const consumed by the terminator so it is reachable.
        let data = graph.create_node(
            NodeKind::IntConst(crate::node::const_value::ConstId::new(7_usize)),
            [],
            [ValueKind::Typed(ValueType::I64)],
        );
        let [data_value] = graph.node_outputs_exact::<1>(data).unwrap();
        let b = graph.create_node(NodeKind::Return, [], []);
        graph.add_node_input(b, c1);
        graph.add_node_input(b, data_value);

        let order: Vec<NodeId> =
            crate::walk::GraphWalkInfo::compute_full(&graph, entry).reverse_postorder(&graph);

        // Entry first.
        assert_eq!(
            order.first(),
            Some(&entry),
            "RPO must start at entry: {order:?}"
        );
        // Each reachable node exactly once.
        let unique: HashSet<NodeId> = order.iter().copied().collect();
        assert_eq!(
            order.len(),
            unique.len(),
            "no node visited twice: {order:?}"
        );
        for n in [entry, a, b, data] {
            assert!(unique.contains(&n), "{n:?} missing from RPO: {order:?}");
        }
    }

    /// A kind filter (`Region` only) yields just the regions, preserving
    /// their relative RPO order (the earlier Region precedes the later).
    #[test]
    fn reverse_postorder_filter_kind_yields_only_matching_in_order() {
        let mut graph = Graph::new();
        let (entry, c0) = make_entry(&mut graph);
        // entry → A (Region) → B (Region) → ret.
        let (a, c1) = make_ctrl_node(&mut graph, c0);
        let (b, c2) = make_ctrl_node(&mut graph, c1);
        let _ret = make_return(&mut graph, c2);

        let regions: Vec<NodeId> = crate::walk::GraphWalkInfo::compute_full(&graph, entry)
            .reverse_postorder(&graph)
            .into_iter()
            .filter(|&n| matches!(graph.node_kind(n), NodeKind::Region))
            .collect();
        assert_eq!(
            regions,
            vec![a, b],
            "only Regions, earlier before later: {regions:?}"
        );
    }

    /// Unreachable nodes are excluded from the RPO.
    #[test]
    fn reverse_postorder_filter_excludes_unreachable() {
        let mut graph = Graph::new();
        let (entry, _c0) = make_entry(&mut graph);
        let isolated = graph.create_node(NodeKind::Return, [], []);

        let order: Vec<NodeId> =
            crate::walk::GraphWalkInfo::compute_full(&graph, entry).reverse_postorder(&graph);
        assert!(order.contains(&entry));
        assert!(
            !order.contains(&isolated),
            "unreachable node must be excluded"
        );
    }

    /// Global RPO is deterministic: two calls on the same graph yield the
    /// identical order, and `entry` is always first.
    #[test]
    fn reverse_postorder_filter_is_deterministic_entry_first() {
        let mut graph = Graph::new();
        let (entry, e_ctrl) = make_entry(&mut graph);
        let a = graph.create_node(
            NodeKind::IntConst(crate::node::const_value::ConstId::new(5_usize)),
            [],
            [ValueKind::Typed(ValueType::I64)],
        );
        let [a_value] = graph.node_outputs_exact::<1>(a).unwrap();
        let c = graph.create_node(
            NodeKind::IntConst(crate::node::const_value::ConstId::new(4_usize)),
            [],
            [ValueKind::Typed(ValueType::I64)],
        );
        let [c_value] = graph.node_outputs_exact::<1>(c).unwrap();
        let add = graph.create_node(
            NodeKind::IntBinaryOp(crate::IntBinaryOp::Add),
            [a_value, c_value],
            [ValueKind::Typed(ValueType::I64)],
        );
        let [add_value] = graph.node_outputs_exact::<1>(add).unwrap();
        let ret = graph.create_node(NodeKind::Return, [], []);
        graph.add_node_input(ret, e_ctrl);
        graph.add_node_input(ret, add_value);

        let order1: Vec<NodeId> =
            crate::walk::GraphWalkInfo::compute_full(&graph, entry).reverse_postorder(&graph);
        let order2: Vec<NodeId> =
            crate::walk::GraphWalkInfo::compute_full(&graph, entry).reverse_postorder(&graph);
        assert_eq!(order1, order2, "RPO must be deterministic");
        assert_eq!(order1[0], entry, "entry first: {order1:?}");
        // Every reachable node present exactly once.
        for n in [entry, a, c, add, ret] {
            assert_eq!(
                order1.iter().filter(|&&x| x == n).count(),
                1,
                "{n:?} must appear exactly once: {order1:?}"
            );
        }
    }

    /// General no-duplicate-visit property: build a richer graph
    /// (diamond + data + back-edge approximation) and assert that
    /// `walk_graph` visits every reachable node at most once.  The
    /// existing inline tests cover specific shapes (single, linear,
    /// diamond); this test pins the general invariant on a less
    /// regular shape.
    #[test]
    fn walk_visits_no_node_more_than_once() {
        use std::collections::HashSet;
        let mut graph = Graph::new();
        let (entry, e_ctrl) = make_entry(&mut graph);
        // entry → A → B (linear).
        let (a, a_ctrl) = make_ctrl_node(&mut graph, e_ctrl);
        let (b, b_ctrl) = make_ctrl_node(&mut graph, a_ctrl);
        // Pure data node referenced as Return's value input.
        let data = graph.create_node(
            NodeKind::IntConst(crate::node::const_value::ConstId::new(0_usize)),
            [],
            [ValueKind::Typed(ValueType::I64)],
        );
        let [data_value] = graph.node_outputs_exact::<1>(data).unwrap();
        let ret = graph.create_node(NodeKind::Return, [], []);
        graph.add_node_input(ret, b_ctrl);
        graph.add_node_input(ret, data_value);

        let visited: Vec<NodeId> = walk_graph(&graph, entry).collect();
        let unique: HashSet<NodeId> = visited.iter().copied().collect();
        assert_eq!(
            visited.len(),
            unique.len(),
            "walk_graph must visit each node at most once: visited={visited:?}"
        );
        // All 5 reachable nodes (entry, a, b, data, ret) must appear.
        for nid in [entry, a, b, data, ret] {
            assert!(unique.contains(&nid), "missing {nid:?}");
        }
    }

    /// Post-order over a control DIAMOND (entry forks to two regions that
    /// re-join at a Return) visits each of the four nodes exactly once,
    /// covers exactly the control-aware reachable set, and puts the lone
    /// root (entry) last — converging control edges must not duplicate the
    /// join.
    #[test]
    fn postorder_on_control_diamond_visits_each_node_once() {
        let mut graph = Graph::new();
        let entry = graph.create_node(
            NodeKind::Entry,
            [],
            [ValueKind::Control, ValueKind::Control],
        );
        let [ctrl_l, ctrl_r] = graph.node_outputs_exact::<2>(entry).unwrap();
        let (left, left_ctrl) = make_ctrl_node(&mut graph, ctrl_l);
        let (right, right_ctrl) = make_ctrl_node(&mut graph, ctrl_r);
        let merge = graph.create_node(NodeKind::Return, [], []);
        graph.add_node_input(merge, left_ctrl);
        graph.add_node_input(merge, right_ctrl);

        let info = GraphWalkInfo::compute_full(&graph, entry);
        let order: Vec<NodeId> = info.postorder(&graph).collect();
        assert_eq!(
            order.len(),
            4,
            "diamond postorder yields exactly 4 nodes: {order:?}"
        );
        for n in [entry, left, right, merge] {
            assert_eq!(
                order.iter().filter(|&&x| x == n).count(),
                1,
                "{n:?} must appear exactly once: {order:?}"
            );
        }
        assert_eq!(
            *order.last().unwrap(),
            entry,
            "the lone root (entry) comes last in post-order"
        );
    }

    /// Walking from a MID-graph seed reaches only that node's cone —
    /// its transitive data operands — never its consumers or the function
    /// spine: a data node has no forward control edges, and use-edges are
    /// not followed.
    #[test]
    fn walk_from_mid_graph_node_reaches_only_its_cone() {
        let mut graph = Graph::new();
        let (entry, e_ctrl) = make_entry(&mut graph);
        let (k1, k1v) = int_const(&mut graph, 1);
        let (k2, k2v) = int_const(&mut graph, 2);
        let (add, addv) = int_bin(&mut graph, crate::IntBinaryOp::Add, k1v, k2v);
        // Return consumes both the control spine and the Add's value.
        let ret = graph.create_node(NodeKind::Return, [], []);
        graph.add_node_input(ret, e_ctrl);
        graph.add_node_input(ret, addv);

        use cranelift_entity::EntityRef;
        let mut cone: Vec<NodeId> = walk_graph(&graph, add).collect();
        cone.sort_unstable_by_key(|n| n.index());
        let mut expected = vec![add, k1, k2];
        expected.sort_unstable_by_key(|n| n.index());
        assert_eq!(
            cone, expected,
            "walk_from(add) covers exactly {{add, k1, k2}} — neither the \
             Return consumer nor the entry spine"
        );
        assert!(!cone.contains(&ret) && !cone.contains(&entry));
    }

    // ── memory_reachable ──────────────────────────────────────────────────────

    /// Constructs a minimal `FunctionBuilder` with no tracked variables and one
    /// active entry region. Mirrors the local `empty_builder` /
    /// `builder_with_region` helpers duplicated per test module in this crate
    /// (`builder/tests.rs`, `builder/build_trait.rs`) — a dev-dep on
    /// `strider-ir-test-utils` would double-compile a DIFFERENT
    /// `FunctionBuilder` under `cargo test`, so each in-crate test module
    /// grows its own tiny copy instead.
    fn builder_with_region() -> crate::Result<crate::FunctionBuilder> {
        let mut b = crate::FunctionBuilder::new(
            vec![],
            strider_target::BuiltCallingConvention::default(),
            strider_target::Endianness::Little,
        )?;
        let r = b.create_region_all()?;
        b.set_entry_region_all(r)?;
        b.set_region(r);
        Ok(b)
    }

    /// `memory_reachable` must walk the InitialMemory -> Store -> Load chain:
    /// every returned node touches memory, InitialMemory is the (included)
    /// root, and the Store is found. A pure-arithmetic node with no memory
    /// edge must not appear.
    #[test]
    fn memory_reachable_covers_the_store_load_chain() {
        use crate::IRBuilderExt;

        let mut b = builder_with_region().unwrap();

        let space = rsleigh::VnSpace::RAM;
        let addr = b.build_int_const(0x1000u64, ValueType::I64).unwrap();
        let data = b.build_int_const(7u64, ValueType::I32).unwrap();
        b.build_store(addr, data, space).unwrap();
        b.build_load(addr, space, ValueType::I32).unwrap();

        // Pure-arithmetic node with no memory edge — must NOT appear.
        let (arith, _) = int_bin(
            b.function_mut().graph_mut(),
            crate::IntBinaryOp::Add,
            addr,
            data,
        );

        // Terminate: without a terminator consuming the region's final
        // memory token, nothing forward-control-reachable ever backward-data
        // -walks into the Store/MemPhi/InitialMemory chain, so
        // `GraphWalkInfo::compute_full` (which `memory_reachable` uses to
        // locate the root) would never find `InitialMemory` at all.
        b.build_return(None, &[]).unwrap();

        let entry = b.entry();
        let f = b.function();
        let mem = memory_reachable(f, entry);

        for &n in &mem {
            let k = f.node_kind(n);
            assert!(
                matches!(
                    k,
                    NodeKind::InitialMemory
                        | NodeKind::Store(_)
                        | NodeKind::Load(_)
                        | NodeKind::Call
                        | NodeKind::CallOther { .. }
                        | NodeKind::MemPhi
                ),
                "non-memory-touching node {n:?} ({k:?}) must not appear in memory_reachable"
            );
        }
        assert!(
            mem.iter()
                .any(|&n| matches!(f.node_kind(n), NodeKind::InitialMemory)),
            "InitialMemory root must be included: {mem:?}"
        );
        assert!(
            mem.iter()
                .any(|&n| matches!(f.node_kind(n), NodeKind::Store(_))),
            "the Store must be reachable: {mem:?}"
        );
        assert!(
            mem.iter()
                .any(|&n| matches!(f.node_kind(n), NodeKind::Load(_))),
            "the Load must be reachable: {mem:?}"
        );
        assert!(
            !mem.contains(&arith),
            "a pure-arithmetic node with no memory edge must not appear: {mem:?}"
        );
    }

    /// Even with zero `Store`/`Load`/`Call` in the function body,
    /// `set_entry_region_all` wires the entry region's `MemPhi` to
    /// `InitialMemory` (`link_memory_regions`), so `InitialMemory` is always
    /// reachable and `memory_reachable` returns exactly the two-node
    /// `[InitialMemory, MemPhi]` chain rather than an empty `Vec`.
    #[test]
    fn memory_reachable_finds_the_entry_mem_phi_with_no_memory_ops() {
        let mut b = builder_with_region().unwrap();
        b.build_return(None, &[]).unwrap();
        let entry = b.entry();
        let f = b.function();
        let mem = memory_reachable(f, entry);

        assert!(
            mem.iter()
                .any(|&n| matches!(f.node_kind(n), NodeKind::InitialMemory)),
            "InitialMemory root must be included: {mem:?}"
        );
        assert!(
            mem.iter()
                .any(|&n| matches!(f.node_kind(n), NodeKind::MemPhi)),
            "the entry region's MemPhi must be reachable: {mem:?}"
        );
        for &n in &mem {
            assert!(
                matches!(f.node_kind(n), NodeKind::InitialMemory | NodeKind::MemPhi),
                "no Store/Load/Call exists yet non-memory node {n:?} appeared: {mem:?}"
            );
        }
    }

    /// `memory_reachable`'s walk must terminate — and dedup correctly — on a
    /// genuinely cyclic memory chain: a loop-header `MemPhi` (`r1`) with a
    /// memory predecessor (`r2`'s `MemPhi`) that is itself reachable
    /// FORWARD from `r1`'s own `MemPhi` output (the loop-continue arm `r1 ->
    /// r2`), which then flows back into `r1` as its back-edge predecessor
    /// (`r2 -> r1`). This is a real back-edge cycle, not merely a diamond:
    /// `r1`'s `MemPhi` output reaches `r2`'s `MemPhi`, which feeds back into
    /// `r1`'s `MemPhi` as an input, closing the loop the `seen` set must
    /// break.
    ///
    /// Shape: `r0` (entry) branches to loop header `r1`; `r1` conditionally
    /// branches to loop body `r2` (continue) or exit `r3` (`build_if`); `r2`
    /// branches back to `r1` (the back-edge); `r3` returns. No Store/Load/Call
    /// is needed — the memory-token plumbing alone (every region's `MemPhi`)
    /// is enough to construct the cycle from the raw builder API.
    #[test]
    fn memory_reachable_terminates_on_a_loop_header_mem_phi_cycle() {
        use crate::IRBuilderExt;
        use std::collections::HashSet;

        let mut b = crate::FunctionBuilder::new(
            vec![],
            strider_target::BuiltCallingConvention::default(),
            strider_target::Endianness::Little,
        )
        .unwrap();

        let r0 = b.create_region_all().unwrap();
        b.set_entry_region_all(r0).unwrap();
        b.set_region(r0);

        // Loop header.
        let r1 = b.create_region_all().unwrap();
        b.build_branch(r1).unwrap();

        b.set_region(r1);
        let r2 = b.create_region_all().unwrap(); // loop body
        let r3 = b.create_region_all().unwrap(); // exit
        let cond = b.build_int_const(1u64, ValueType::I1).unwrap();
        b.build_if(cond, r2, r3).unwrap();

        // Back-edge: loop body branches back to the header, wiring r2's
        // MemPhi output as r1's MemPhi's second predecessor.
        b.set_region(r2);
        b.build_branch(r1).unwrap();

        b.set_region(r3);
        b.build_return(None, &[]).unwrap();

        let entry = b.entry();
        let f = b.function();

        // Termination: this call must return rather than hang/overflow the
        // stack despite the r1 <-> r2 MemPhi back-edge. Reaching the
        // assertions below IS the termination proof.
        let mem = memory_reachable(f, entry);

        assert!(
            mem.iter()
                .filter(|&&n| matches!(f.node_kind(n), NodeKind::MemPhi))
                .count()
                >= 2,
            "both the loop-header MemPhi (r1) and the loop-body MemPhi (r2) \
             forming the back-edge must be included: {mem:?}"
        );
        // No duplicates despite the back-edge revisiting r1's MemPhi.
        let unique: HashSet<NodeId> = mem.iter().copied().collect();
        assert_eq!(
            mem.len(),
            unique.len(),
            "no node visited twice despite the back-edge cycle: {mem:?}"
        );
    }
}
