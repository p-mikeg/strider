//! Read vocabulary the matcher and the instantiation walk need on top of the
//! generic [`strider_graph::Graph`], shared by the match side
//! ([`Pattern`](crate::matcher::Pattern)) and the build side
//! ([`Template`](crate::template::Template)).
//!
//! ## The sparse-slot bridge
//!
//! Pattern inputs are **sparse**: `call().arg(0, ...)` wires only raw input
//! slot 4. The generic graph stores inputs densely, so each input's original
//! consumer slot rides on the node payload ([`HasInputSlots::input_slots`]),
//! parallel to the generic graph's input order.
//! [`consumed_inputs`](PatGraphRead::consumed_inputs) zips the two back
//! together.

use anyhow::anyhow;
use petgraph::visit::{DfsPostOrder, Reversed, Walker};
use rustc_hash::FxHashSet;
use strider_graph::{Graph, NeverCacheable, NodeId, ValueId, Vertex};

pub(crate) trait HasInputSlots {
    /// Consumer slots, one per input, parallel to the generic graph's input
    /// order.
    fn input_slots(&self) -> &[usize];
}

pub(crate) trait PatGraphRead<N: HasInputSlots, V> {
    fn node_weight(&self, node: NodeId) -> &N;
    fn output_weight(&self, value: ValueId) -> &V;
    fn producer_of(&self, value: ValueId) -> NodeId;
    /// Recovers each input's sparse consumer slot from the node payload.
    fn consumed_inputs(&self, node: NodeId) -> Vec<(usize, ValueId)>;
    /// Borrows the generic graph's contiguous output slice, so the matcher
    /// and instantiate hot paths allocate nothing per node.
    fn produced_outputs(&self, node: NodeId) -> &[ValueId];
    /// The root is the unique sink, derived structurally rather than stored.
    ///
    /// # Errors
    /// Unless there is exactly one sink: zero means rootless or cyclic, more
    /// than one means multi-rooted.
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

    fn produced_outputs(&self, node: NodeId) -> &[ValueId] {
        self.node_outputs(node)
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
            [] => Err(anyhow!(
                "pattern graph has no sink node (rootless or cyclic)"
            )),
            many => Err(anyhow!(
                "pattern graph has {} sink nodes; expected exactly one (multi-rooted)",
                many.len()
            )),
        }
    }
}

/// `root` plus its transitive input cone, in producer-before-consumer order.
///
/// Reachability walks reversed edges from `root`; the global toposort is then
/// filtered to that set and projected back to node vertices. Errors on a
/// cycle.
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
