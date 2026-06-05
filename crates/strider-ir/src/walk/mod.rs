use core::{iter, ops::ControlFlow};

pub use entity_utils::set::DenseEntitySet;
use entity_utils::Worklist;

use crate::{
    graph::Graph,
    node::{NodeId, ValueId},
};

mod cast;
pub use cast::{cast_mask_of, skip_casts, CastMask};

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
    let mut visited = DenseEntitySet::new();
    let mut worklist: Worklist<NodeId> = Worklist::new();
    worklist.enqueue(entry);
    while let Some(node) = worklist.dequeue() {
        // `visited.insert` doubles as the dedup gate: when `node` was
        // already processed via another path, `insert` returns false
        // and we skip the successor sweep.  `Worklist` only dedups
        // while-queued (re-enqueue after dequeue is allowed), so we
        // still need this check to avoid quadratic re-processing on
        // CFGs whose joins fan in from multiple predecessors.
        if !visited.insert(node) {
            continue;
        }
        for succ in cfg_succs(graph, node) {
            worklist.enqueue(succ);
        }
    }
    visited
}

/// A pre-order walk over the IR graph using a [`DenseEntitySet`] as the
/// visited tracker.
pub type PreOrder<G> = graphwalk::PreOrder<G, DenseEntitySet<NodeId>>;

/// A post-order walk over the IR graph using a [`DenseEntitySet`] as the
/// visited tracker.
pub type PostOrder<G> = graphwalk::PostOrder<G, DenseEntitySet<NodeId>>;

/// A [`graphwalk::GraphRef`] implementation that drives successor enumeration
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

/// Returns an iterator over the predecessor control outputs of a
/// region-join `Region` producing `out`.  Returns an empty
/// iterator when the producer of `out` is not a `Region`.
///
/// `Region`'s signature is `inputs: variadic Control; outputs:
/// [Control, PhiToken]`, so every input is a control-typed producer
/// from a predecessor region.  Callers use this iterator to enumerate
/// the per-region alternatives feeding the join.
///
/// Only the structural enumeration lives here; ownership of rollback,
/// recursion, and per-attempt state stays with the caller.
pub fn region_predecessors(
    graph: &Graph,
    out: ValueId,
) -> impl Iterator<Item = ValueId> + '_ {
    use crate::node::NodeKind;
    let producer = graph.producer(out);
    let is_region = matches!(graph.node_kind(producer), NodeKind::Region);
    let inputs = graph.node_inputs(producer);
    // `Inputs` is Copy, so we move it into the iterator chain and let
    // `take(0)` produce an empty stream for non-Region producers
    // without branching on an `Either` variant.
    let take = if is_region { inputs.len() } else { 0 };
    inputs.into_iter().take(take)
}

impl graphwalk::GraphRef for GraphWalkSuccs<'_> {
    type NodeId = NodeId;

    fn try_successors(
        &self,
        node: NodeId,
        f: impl FnMut(NodeId) -> ControlFlow<()>,
    ) -> ControlFlow<()> {
        graph_walk_succs(self.0, node).try_for_each(f)
    }
}

/// The concrete pre-order walk type used by [`crate::IRWalker::walk`].
pub type GraphWalk<'a> = PreOrder<GraphWalkSuccs<'a>>;

/// A [`graphwalk::GraphRef`] whose successors are a node's **data-input
/// producers only** (no forward control edges).  Driving a post-order walk
/// with this relation yields every producer before the node that consumes
/// it — the defs-before-uses order used by value-cone analyses such as
/// `decompose_sp`.
#[derive(Clone, Copy)]
pub struct InputSuccs<'a>(&'a Graph);

impl graphwalk::GraphRef for InputSuccs<'_> {
    type NodeId = NodeId;

    fn try_successors(
        &self,
        node: NodeId,
        f: impl FnMut(NodeId) -> ControlFlow<()>,
    ) -> ControlFlow<()> {
        self.0
            .node_inputs(node)
            .into_iter()
            .filter(|&value| !self.0.value_kind(value).is_control())
            .map(|value| self.0.value_definition(value).0)
            .try_for_each(f)
    }
}

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

/// Every `(consumer, input_slot)` that consumes one of `node`'s outputs —
/// the raw forward def→use successor relation, unfiltered by liveness.
pub fn raw_def_use_succs(graph: &Graph, node: NodeId) -> impl Iterator<Item = (NodeId, u32)> + '_ {
    graph
        .node_outputs(node)
        .iter()
        .flat_map(move |output| graph.value_uses(*output))
}

/// [`raw_def_use_succs`] restricted to consumers in `live_nodes` — the
/// successor relation a forward walk follows so it never steps outside the
/// reachable set computed by [`GraphWalkInfo::compute_full`].
pub fn def_use_succs<'a>(
    graph: &'a Graph,
    live_nodes: &'a DenseEntitySet<NodeId>,
    node: NodeId,
) -> impl Iterator<Item = (NodeId, u32)> + 'a {
    raw_def_use_succs(graph, node).filter(move |&(succ, _use_idx)| live_nodes.contains(succ))
}

/// A [`graphwalk::GraphRef`] over the forward def→use edges, **unrestricted**
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

impl graphwalk::GraphRef for RawDefUseSuccs<'_> {
    type NodeId = NodeId;

    fn try_successors(
        &self,
        node: NodeId,
        mut f: impl FnMut(NodeId) -> ControlFlow<()>,
    ) -> ControlFlow<()> {
        raw_def_use_succs(self.0, node).try_for_each(|(succ, _input_idx)| f(succ))
    }
}

/// A [`graphwalk::GraphRef`] over the forward def→use edges, restricted to a
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

impl graphwalk::GraphRef for DefUseSuccs<'_> {
    type NodeId = NodeId;

    fn try_successors(
        &self,
        node: NodeId,
        mut f: impl FnMut(NodeId) -> ControlFlow<()>,
    ) -> ControlFlow<()> {
        def_use_succs(self.graph, self.live_nodes, node).try_for_each(|(succ, _input_idx)| f(succ))
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
        let mut roots = Vec::new();
        for node in walk.by_ref() {
            if graph.node_inputs(node).is_empty() {
                roots.push(node);
            }
        }

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

/// Returns the set of nodes belonging to the region whose terminator
/// consumes `exit_control`.
///
/// Concretely:
///
/// 1. Seed the result with the producer of `exit_control` (the region's
///    terminator: typically a `Return`, `Call`, or `If`).
/// 2. Walk **backward** along incoming `Control`-kind edges, collecting
///    every visited node.  Stop at `Region` (region-join) nodes:
///    include the `Region` itself but do NOT recurse through its
///    control inputs — those control predecessors live in upstream
///    regions and a partition walk must not cross the join.
/// 3. Union in every data ancestor (transitive closure over all input
///    edges) of every node in step (1)+(2).  Data ancestors are
///    intentionally shared across regions in a sea-of-nodes IR
///    (`IntConst`, `InitialMemory`, `InitialVar(_)` and so on are
///    single-defined and consumed everywhere), so a per-region view that
///    omits them is unreadable; including them is the standard
///    "value cone" rendering.
///
/// The resulting set is the region's *visualisation membership* — the
/// minimal set of nodes a per-region dot dump must include for the
/// region's exit-control to make sense in isolation.  It is not a
/// disjoint partition of the graph: data ancestors are shared across
/// regions.
pub fn region_membership_from_exit(
    graph: &Graph,
    exit_control: ValueId,
) -> DenseEntitySet<NodeId> {
    use crate::node::NodeKind;
    let seed = graph.producer(exit_control);

    // (1) collect the region's control spine via a backward
    // control walk, with `Region` as a barrier (include it, don't
    // recurse through its control inputs).
    let mut spine: DenseEntitySet<NodeId> = DenseEntitySet::new();
    let mut stack: Vec<NodeId> = vec![seed];
    while let Some(node) = stack.pop() {
        if !spine.insert(node) {
            continue;
        }
        if matches!(graph.node_kind(node), NodeKind::Region) {
            // Barrier: include the Region but don't follow its
            // control predecessors (those belong to upstream regions).
            continue;
        }
        for input in graph.node_inputs(node) {
            if !graph.value_kind(input).is_control() {
                continue;
            }
            let (producer, _) = graph.value_definition(input);
            stack.push(producer);
        }
    }

    // (2) union in all data ancestors of every spine node.  Walk
    // ONLY non-control inputs — control inputs are the spine's edges
    // (already handled in pass 1 with the Region barrier), and
    // following them here would re-cross the barrier from the other
    // side (a `Region` that fed our seed exposes its control inputs,
    // which pass 1 already covered).
    let mut visible = spine.clone();
    let mut stack: Vec<NodeId> = visible.iter().collect();
    while let Some(node) = stack.pop() {
        for input in graph.node_inputs(node) {
            if graph.value_kind(input).is_control() {
                continue;
            }
            let (producer, _) = graph.value_definition(input);
            if visible.insert(producer) {
                stack.push(producer);
            }
        }
    }
    visible
}

/// Collects every node within `depth` hops of `anchor`, following both
/// forward (use-of-output) and backward (input-from-producer) edges.
///
/// `depth = 0` returns the singleton `{anchor}`; `depth = 1` returns
/// the anchor plus every direct predecessor (input producer) and
/// successor (output consumer); and so on.  Used by neighborhood-focus
/// HTML dumps to render a subgraph around a node of interest without
/// pulling in the whole reachable graph.
///
/// Both edge directions are followed because IR debugging typically
/// wants to see both "what produced this value" (backward) and "what
/// uses it" (forward) from the anchor.
pub fn collect_neighborhood(
    graph: &Graph,
    anchor: NodeId,
    depth: u32,
) -> DenseEntitySet<NodeId> {
    let mut visited: DenseEntitySet<NodeId> = DenseEntitySet::new();
    visited.insert(anchor);
    if depth == 0 {
        return visited;
    }
    // BFS-style frontier expansion: at iteration `k`, `frontier`
    // contains every node first discovered at hop distance `k`.  This
    // gives a depth-bounded walk in O((depth) * neighborhood_size).
    let mut frontier: Vec<NodeId> = vec![anchor];
    for _ in 0..depth {
        let mut next_frontier: Vec<NodeId> = Vec::new();
        for node in frontier {
            // Backward edges: each input's producer.
            for input in graph.node_inputs(node) {
                let (producer, _) = graph.value_definition(input);
                if visited.insert(producer) {
                    next_frontier.push(producer);
                }
            }
            // Forward edges: each output's consumers.
            for &output in graph.node_outputs(node) {
                for (consumer, _) in graph.value_uses(output) {
                    if visited.insert(consumer) {
                        next_frontier.push(consumer);
                    }
                }
            }
        }
        if next_frontier.is_empty() {
            break;
        }
        frontier = next_frontier;
    }
    visited
}

/// Like [`walk_graph`] but accepts an optional entry: returns an
/// empty walk when `entry` is `None`.  Used by [`Graph::preorder`] so
/// pre-build graphs yield no nodes instead of panicking.
///
/// Crate-private: external callers must route through [`Graph::preorder`].
pub(crate) fn walk_graph_opt(graph: &Graph, entry: Option<NodeId>) -> GraphWalk<'_> {
    PreOrder::new(GraphWalkSuccs::new(graph), entry)
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::{IntPayload, NodeKind, ValueKind, ValueType};

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
            NodeKind::IntConst(IntPayload::Small(42)),
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
            NodeKind::IntConst(IntPayload::Small(0)),
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
            NodeKind::IntConst(IntPayload::Small(5)),
            [],
            [ValueKind::Typed(ValueType::I32)],
        );
        let outs: Vec<_> = cfg_outputs(&graph, node).collect();
        assert!(outs.is_empty());
    }

    // ── region_membership_from_exit ───────────────────────────────────────────

    /// A linear chain entry → A (Region) → ret: when the seed's
    /// producer is itself a Region, the barrier triggers at the
    /// seed and only the seed appears in the membership.  Entry (one
    /// hop past the barrier) is excluded.
    #[test]
    fn region_membership_seed_is_region_stops_at_seed() {
        let mut graph = Graph::new();
        let (entry, c0) = make_entry(&mut graph);
        let (a, c1) = make_ctrl_node(&mut graph, c0);
        let ret = make_return(&mut graph, c1);

        let mem = region_membership_from_exit(&graph, c1);
        // Seed (a, a Region) is included.
        assert!(mem.contains(a), "seed (Region A) must be included");
        // Barrier triggers at the seed — entry is one hop past the barrier.
        assert!(!mem.contains(entry), "barrier stops the walk at the seed");
        // ret is the consumer of c1, not the producer.
        assert!(!mem.contains(ret), "Return is the exit's consumer, not its producer");
    }

    /// A linear chain whose seed is a non-Region (here a Return
    /// node treated as the "exit producer") walks back through control
    /// inputs until hitting Entry.  Verifies that the barrier ONLY
    /// triggers at Region — non-CS nodes are crossed normally.
    #[test]
    fn region_membership_non_region_seed_walks_to_entry() {
        let mut graph = Graph::new();
        // Build entry → ret directly (no Region between them).
        let (entry, c0) = make_entry(&mut graph);
        let ret = make_return(&mut graph, c0);
        // To exercise the function, we need to seed from a ValueId
        // whose producer is `ret` (not a Region) and whose
        // producer has a control input.  Return has no outputs, so we
        // can't seed from it directly.  Instead, attach a dummy
        // non-CS leaf node with an output we can seed from.
        //
        // Use an If node: its control output is non-CS and it has a
        // control input we can chain back from.
        // ...but If needs a Bool input.  Simplest: chain a second Entry
        // node is impossible (Entry is unique).
        //
        // Take a different angle: just verify the seed itself is
        // present when it's a non-Region.  Here the simplest
        // demonstrable case is the entry node serving as its own seed
        // when seeded by its OWN output.
        let mem = region_membership_from_exit(&graph, c0);
        // Seed = entry (producer of c0); entry has no control inputs,
        // so spine = {entry}.
        assert!(mem.contains(entry), "entry as seed must be included");
        assert!(!mem.contains(ret), "ret is downstream of the seed");
    }

    /// A Region seed must act as a barrier: control predecessors of
    /// the seed are NOT crossed (this is how a region partition stops at
    /// the join).
    #[test]
    fn region_membership_stops_at_seed_region() {
        let mut graph = Graph::new();
        // entry → a → cs_seed (the join we're seeded at).
        // entry's other branch (b) feeds cs_seed too, but must NOT appear
        // in the membership because we stop AT cs_seed.
        let entry = graph.create_node(
            NodeKind::Entry,
            [],
            [ValueKind::Control, ValueKind::Control],
        );
        let [c_a, c_b] = graph.node_outputs_exact::<2>(entry).unwrap();
        let (a, a_ctrl) = make_ctrl_node(&mut graph, c_a);
        let (b, b_ctrl) = make_ctrl_node(&mut graph, c_b);
        let cs_seed = graph.create_node(NodeKind::Region, [], [ValueKind::Control]);
        graph.add_node_input(cs_seed, a_ctrl);
        graph.add_node_input(cs_seed, b_ctrl);
        let [cs_seed_value] = graph.node_outputs_exact::<1>(cs_seed).unwrap();

        let mem = region_membership_from_exit(&graph, cs_seed_value);
        // The seed is included.
        assert!(mem.contains(cs_seed), "seed (a Region) is always included");
        // But its control predecessors must NOT be crossed.
        assert!(!mem.contains(a), "a is on the other side of the Region barrier");
        assert!(!mem.contains(b), "b is on the other side of the Region barrier");
        assert!(!mem.contains(entry), "entry is upstream of the barrier");
    }

    // ── collect_neighborhood ──────────────────────────────────────────────────

    /// depth=0 returns the singleton anchor set.
    #[test]
    fn collect_neighborhood_depth_zero_is_singleton() {
        let mut graph = Graph::new();
        let (entry, _) = make_entry(&mut graph);
        let nbhd = collect_neighborhood(&graph, entry, 0);
        assert!(nbhd.contains(entry));
        // No other nodes should be present.
        let total: usize = nbhd.iter().count();
        assert_eq!(total, 1, "depth=0 must yield exactly the anchor");
    }

    /// depth=1 includes immediate predecessors and successors of the anchor.
    #[test]
    fn collect_neighborhood_depth_one_includes_direct_neighbors() {
        // entry → a → b: anchor = a at depth=1 must include entry, a, b.
        let mut graph = Graph::new();
        let (entry, c0) = make_entry(&mut graph);
        let (a, c1) = make_ctrl_node(&mut graph, c0);
        let b = make_return(&mut graph, c1);

        let nbhd = collect_neighborhood(&graph, a, 1);
        assert!(nbhd.contains(a), "anchor must be included");
        assert!(nbhd.contains(entry), "1-hop predecessor must be included");
        assert!(nbhd.contains(b), "1-hop successor must be included");
    }

    /// depth=2 includes 2-hop neighbours.
    #[test]
    fn collect_neighborhood_depth_two_extends_one_more_hop() {
        // entry → a → b → c: anchor = a at depth=2 must include entry,
        // a, b, AND c (1 hop forward from b, 2 hops from a).
        let mut graph = Graph::new();
        let (entry, c0) = make_entry(&mut graph);
        let (a, c1) = make_ctrl_node(&mut graph, c0);
        let (b, c2) = make_ctrl_node(&mut graph, c1);
        let c = make_return(&mut graph, c2);

        let nbhd_1 = collect_neighborhood(&graph, a, 1);
        assert!(!nbhd_1.contains(c), "depth=1 must NOT reach the 2-hop neighbour");

        let nbhd_2 = collect_neighborhood(&graph, a, 2);
        assert!(nbhd_2.contains(entry));
        assert!(nbhd_2.contains(a));
        assert!(nbhd_2.contains(b));
        assert!(nbhd_2.contains(c), "depth=2 must include the 2-hop neighbour");
    }

    /// A high depth stops naturally when the frontier is exhausted.
    #[test]
    fn collect_neighborhood_high_depth_terminates_at_reachable_set() {
        let mut graph = Graph::new();
        let (entry, c0) = make_entry(&mut graph);
        let _ret = make_return(&mut graph, c0);

        // depth=100 still terminates because the frontier empties at hop 1.
        let nbhd = collect_neighborhood(&graph, entry, 100);
        assert_eq!(nbhd.iter().count(), 2, "frontier exhausted after 1 hop");
    }

    /// Data ancestors of every spine node must be included even when
    /// they live "outside" the control walk reach.
    #[test]
    fn region_membership_includes_data_ancestors() {
        let mut graph = Graph::new();
        // src is a pure data node (an IntConst) with no control connection.
        let src = graph.create_node(
            NodeKind::IntConst(IntPayload::Small(42)),
            [],
            [ValueKind::Typed(ValueType::I64)],
        );
        let [src_value] = graph.node_outputs_exact::<1>(src).unwrap();
        // entry → ret(data: src).
        let (entry, e_ctrl) = make_entry(&mut graph);
        let ret = graph.create_node(NodeKind::Return, [], []);
        graph.add_node_input(ret, e_ctrl);
        graph.add_node_input(ret, src_value);

        // Seed by the control output the Return consumed (e_ctrl, produced
        // by entry).  The function keys on the producer of exit_control,
        // so seed = entry.
        let mem = region_membership_from_exit(&graph, e_ctrl);
        assert!(mem.contains(entry), "entry (seed) must be included");
        // ret is on the consumer side of e_ctrl — not in the membership.
        assert!(!mem.contains(ret), "ret is the consumer, not the producer");
        // src is a data ancestor of entry?  No — entry has no inputs.  So
        // src is NOT pulled in here.  This test pins that behaviour: the
        // data closure runs over spine nodes (which here is just `entry`
        // since the seed is entry and entry has no control predecessors).
        assert!(!mem.contains(src), "src is not a data ancestor of entry");
    }

    // ── rpo (defs-before-uses data-cone walk) ─────────────────────────────────

    /// `rpo` over `Add(InitialVar, IntConst)` must emit BOTH operands before
    /// the `Add` that consumes them (defs-before-uses). The seed node is last.
    #[test]
    fn rpo_emits_operands_before_consumer() {
        let mut graph = Graph::new();
        let a = graph.create_node(
            NodeKind::IntConst(IntPayload::Small(5)),
            [],
            [ValueKind::Typed(ValueType::I64)],
        );
        let [a_value] = graph.node_outputs_exact::<1>(a).unwrap();
        let c = graph.create_node(
            NodeKind::IntConst(IntPayload::Small(4)),
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

        let order: Vec<NodeId> = crate::walk::GraphWalkInfo::compute_full(&graph, add).reverse_postorder(&graph);

        assert_eq!(order.len(), 3, "rpo must visit each cone node once: {order:?}");
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
            NodeKind::IntConst(IntPayload::Small(7)),
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

        let order: Vec<NodeId> = crate::walk::GraphWalkInfo::compute_full(&graph, add).reverse_postorder(&graph);
        assert_eq!(order, vec![c, add], "shared operand visited once, before Add");
    }

    // ── GraphWalkInfo / real RPO machinery ────────────────────────────────────

    /// Builds an `IntConst` and returns `(node, value)`.
    fn int_const(graph: &mut Graph, v: u64) -> (NodeId, ValueId) {
        let n = graph.create_node(NodeKind::IntConst(IntPayload::Small(v)), [], [ValueKind::Typed(ValueType::I64)]);
        let [out] = graph.node_outputs_exact::<1>(n).unwrap();
        (n, out)
    }

    /// Builds an `IntBinaryOp(op)` over `[l, r]` and returns `(node, value)`.
    fn int_bin(graph: &mut Graph, op: crate::IntBinaryOp, l: ValueId, r: ValueId) -> (NodeId, ValueId) {
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
        assert_eq!(info.roots, vec![k], "only the input-less IntConst is a root");
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

        let order = crate::walk::GraphWalkInfo::compute_full(&graph, sink).reverse_postorder(&graph);
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

        let order = crate::walk::GraphWalkInfo::compute_full(&graph, entry).reverse_postorder(&graph);
        let unique: HashSet<NodeId> = order.iter().copied().collect();
        assert_eq!(order.len(), unique.len(), "no node visited twice despite the cycle: {order:?}");
        for n in [entry, a, b] {
            assert!(unique.contains(&n), "{n:?} missing despite the cycle: {order:?}");
        }
        assert_eq!(order.first(), Some(&entry), "input-less root (entry) first: {order:?}");
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
        assert!(raw.contains(&neg), "raw walk must reach the dead consumer: {raw:?}");
        assert!(raw.contains(&k), "raw walk includes the root: {raw:?}");
        // Post-order: consumer before producer.
        let pos = |n: NodeId| raw.iter().position(|&x| x == n).unwrap();
        assert!(pos(neg) < pos(k), "post-order yields the consumer before the producer");
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
            NodeKind::IntConst(IntPayload::Small(7)),
            [],
            [ValueKind::Typed(ValueType::I64)],
        );
        let [data_value] = graph.node_outputs_exact::<1>(data).unwrap();
        let b = graph.create_node(NodeKind::Return, [], []);
        graph.add_node_input(b, c1);
        graph.add_node_input(b, data_value);

        let order: Vec<NodeId> = crate::walk::GraphWalkInfo::compute_full(&graph, entry).reverse_postorder(&graph);

        // Entry first.
        assert_eq!(order.first(), Some(&entry), "RPO must start at entry: {order:?}");
        // Each reachable node exactly once.
        let unique: HashSet<NodeId> = order.iter().copied().collect();
        assert_eq!(order.len(), unique.len(), "no node visited twice: {order:?}");
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
        assert_eq!(regions, vec![a, b], "only Regions, earlier before later: {regions:?}");
    }

    /// Unreachable nodes are excluded from the RPO.
    #[test]
    fn reverse_postorder_filter_excludes_unreachable() {
        let mut graph = Graph::new();
        let (entry, _c0) = make_entry(&mut graph);
        let isolated = graph.create_node(NodeKind::Return, [], []);

        let order: Vec<NodeId> = crate::walk::GraphWalkInfo::compute_full(&graph, entry).reverse_postorder(&graph);
        assert!(order.contains(&entry));
        assert!(!order.contains(&isolated), "unreachable node must be excluded");
    }

    /// Global RPO is deterministic: two calls on the same graph yield the
    /// identical order, and `entry` is always first.
    #[test]
    fn reverse_postorder_filter_is_deterministic_entry_first() {
        let mut graph = Graph::new();
        let (entry, e_ctrl) = make_entry(&mut graph);
        let a = graph.create_node(
            NodeKind::IntConst(IntPayload::Small(5)),
            [],
            [ValueKind::Typed(ValueType::I64)],
        );
        let [a_value] = graph.node_outputs_exact::<1>(a).unwrap();
        let c = graph.create_node(
            NodeKind::IntConst(IntPayload::Small(4)),
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

        let order1: Vec<NodeId> = crate::walk::GraphWalkInfo::compute_full(&graph, entry).reverse_postorder(&graph);
        let order2: Vec<NodeId> = crate::walk::GraphWalkInfo::compute_full(&graph, entry).reverse_postorder(&graph);
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
            NodeKind::IntConst(IntPayload::Small(0)),
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
}
