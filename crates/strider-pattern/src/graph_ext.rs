//! BiGraph-compatible read helpers over the generic
//! [`strider_graph::Graph`].
//!
//! The match side ([`Pattern`](crate::matcher::Pattern)) and the build side
//! ([`Template`](crate::template::Template)) both store their bipartite
//! pattern graph as a `strider_graph::Graph<N, V, NeverCacheable>`. The
//! generic graph exposes structural verbs (`node_inputs`, `node_outputs`,
//! `producer`, `value_kind_ref`, …) but not the BiGraph-era vocabulary the
//! matcher / instantiation walk were written against (`consumed_inputs`
//! with per-edge slots, `produced_outputs`, `derive_root`,
//! `reachable_topo`). This extension trait restores that vocabulary on top
//! of the generic graph so the two consumers read it the same way they read
//! the old `BiGraph`.
//!
//! ## The sparse-slot bridge
//!
//! A pattern's inputs are **sparse** (`call().arg(0, …)` wires only raw
//! input slot 4), whereas the generic graph stores inputs densely. The
//! original consumer slot of each input therefore rides on the node payload
//! ([`HasInputSlots::input_slots`]) — parallel to the generic graph's input
//! order — and [`consumed_inputs`](PatGraphRead::consumed_inputs) zips the
//! two back together to reproduce the BiGraph `Consumes { slot }` edge.

use anyhow::anyhow;
use petgraph::visit::{DfsPostOrder, Reversed, Walker};
use rustc_hash::FxHashSet;
use strider_graph::{Graph, NeverCacheable, NodeId, ValueId, Vertex};

/// A node payload that records the consumer input slot of each of its
/// inputs, parallel to the generic graph's input order.
pub(crate) trait HasInputSlots {
    /// The consumer slots, one per input, in input order.
    fn input_slots(&self) -> &[usize];
}

/// BiGraph-compatible read verbs over a generic pattern / template graph.
pub(crate) trait PatGraphRead<N: HasInputSlots, V> {
    /// The node payload at `node`.
    fn node_weight(&self, node: NodeId) -> &N;
    /// The output (value) payload at `value`.
    fn output_weight(&self, value: ValueId) -> &V;
    /// The producer node of `value`.
    fn producer_of(&self, value: ValueId) -> NodeId;
    /// Every `(consumer_slot, producer_value)` input of `node`, recovering
    /// the sparse consumer slot from the node payload.
    fn consumed_inputs(&self, node: NodeId) -> Vec<(usize, ValueId)>;
    /// The value (output) vertices `node` produces.
    fn produced_outputs(&self, node: NodeId) -> Vec<ValueId>;
    /// The unique **sink** node — a node none of whose produced outputs are
    /// consumed — recovered structurally rather than stored.
    ///
    /// # Errors
    /// Errors unless there is exactly one sink: zero (rootless / cyclic) or
    /// more than one (multi-rooted).
    fn derive_root(&self) -> anyhow::Result<NodeId>;
}

impl<N: HasInputSlots, V> PatGraphRead<N, V> for Graph<N, V, NeverCacheable> {
    fn node_weight(&self, node: NodeId) -> &N {
        self.node_kind(node)
    }

    fn output_weight(&self, value: ValueId) -> &V {
        self.value_kind_ref(value)
    }

    fn producer_of(&self, value: ValueId) -> NodeId {
        self.producer(value)
    }

    fn consumed_inputs(&self, node: NodeId) -> Vec<(usize, ValueId)> {
        let slots = self.node_kind(node).input_slots();
        self.node_inputs(node)
            .into_iter()
            .enumerate()
            .map(|(i, value)| (slots[i], value))
            .collect()
    }

    fn produced_outputs(&self, node: NodeId) -> Vec<ValueId> {
        self.node_outputs(node).to_vec()
    }

    fn derive_root(&self) -> anyhow::Result<NodeId> {
        let sinks: Vec<NodeId> = self
            .all_node_ids()
            .filter(|&node| {
                self.node_outputs(node)
                    .iter()
                    .all(|&out| self.value_uses(out).next().is_none())
            })
            .collect();
        match sinks.as_slice() {
            [only] => Ok(*only),
            [] => Err(anyhow!("pattern graph has no sink node (rootless or cyclic)")),
            many => Err(anyhow!(
                "pattern graph has {} sink nodes; expected exactly one (multi-rooted)",
                many.len()
            )),
        }
    }
}

/// A staged node whose inputs reference producer staged-node indices.
///
/// Implemented by the `StagedNode` types in both the match-side and
/// build-side builders so [`topo_order`] can be shared between them.
pub(crate) trait StagedInputs {
    /// The producer staged-node index for each input, in input order.
    fn input_producer_indices(&self) -> impl Iterator<Item = usize> + '_;
}

/// Kahn topo-sort over staged nodes (producers before consumers).
///
/// Returns the node indices in producer-before-consumer order.
///
/// # Panics
/// Panics if the staged graph contains a cycle (a builder bug — staged
/// pattern/template graphs are always DAGs).
pub(crate) fn topo_order<S: StagedInputs>(nodes: &[S]) -> Vec<usize> {
    let n = nodes.len();
    let mut indeg = vec![0usize; n];
    for (i, node) in nodes.iter().enumerate() {
        indeg[i] = node.input_producer_indices().count();
    }
    let mut order = Vec::with_capacity(n);
    let mut ready: Vec<usize> = (0..n).filter(|&i| indeg[i] == 0).collect();
    while let Some(i) = ready.pop() {
        order.push(i);
        for (j, node) in nodes.iter().enumerate() {
            for prod_idx in node.input_producer_indices() {
                if prod_idx == i {
                    indeg[j] -= 1;
                    if indeg[j] == 0 {
                        ready.push(j);
                    }
                }
            }
        }
    }
    assert_eq!(order.len(), n, "cycle in staged pattern/template graph");
    order
}

/// Returns every node vertex reachable backwards from `root` (i.e. `root`
/// and its transitive input cone) in producer-before-consumer topological
/// order.
///
/// The generic graph's bipartite petgraph view drives the traversal: a
/// `Node → Value → Node` relation means a producer node always precedes its
/// consumer nodes in a `toposort`. Reachability follows reversed edges from
/// the `root` node vertex; the global toposort is filtered to the reachable
/// set, then projected back to node vertices. Errors on a cycle.
pub(crate) fn reachable_topo<N, V>(
    graph: &Graph<N, V, NeverCacheable>,
    root: NodeId,
) -> anyhow::Result<Vec<NodeId>> {
    let root_vtx = Vertex::Node(root);
    let reachable: FxHashSet<Vertex> = DfsPostOrder::new(Reversed(graph), root_vtx)
        .iter(Reversed(graph))
        .collect();
    let sorted = petgraph::algo::toposort(graph, None)
        .map_err(|c| anyhow!("pattern graph cycle at {:?}", c.node_id()))?;
    Ok(sorted
        .into_iter()
        .filter(|v| reachable.contains(v))
        .filter_map(|v| match v {
            Vertex::Node(n) => Some(n),
            Vertex::Value(_) => None,
        })
        .collect())
}
