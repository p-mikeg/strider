//! Pattern inputs are **sparse**: `call().arg(0, ...)` wires only raw input
//! slot 4. The generic graph stores inputs densely, so each input's original
//! consumer slot rides on the node payload ([`HasInputSlots::input_slots`]),
//! parallel to the generic graph's input order.
//! [`consumed_inputs`] zips the two back together.

use anyhow::anyhow;
use petgraph::visit::{DfsPostOrder, Reversed, Walker};
use rustc_hash::FxHashSet;
use strider_graph::{Graph, NeverCacheable, NodeId, ValueId, Vertex};

pub(crate) trait HasInputSlots {
    /// Consumer slots, one per input, parallel to the generic graph's input
    /// order.
    fn input_slots(&self) -> &[usize];
}

/// Recovers each input's sparse consumer slot from the node payload.
pub(crate) fn consumed_inputs<N: HasInputSlots, V>(
    graph: &Graph<N, V, NeverCacheable>,
    node: NodeId,
) -> Vec<(usize, ValueId)> {
    let slots = graph.node_kind(node).input_slots();
    graph
        .node_inputs(node)
        .into_iter()
        .enumerate()
        .map(|(i, value)| (slots[i], value))
        .collect()
}

/// The root is the unique sink, derived structurally rather than stored.
///
/// # Errors
/// Unless there is exactly one sink: zero means rootless or cyclic, more than
/// one means multi-rooted.
pub(crate) fn derive_root<N, V>(graph: &Graph<N, V, NeverCacheable>) -> anyhow::Result<NodeId> {
    let sinks: Vec<NodeId> = graph
        .all_node_ids()
        .filter(|&node| {
            graph
                .node_outputs(node)
                .iter()
                .all(|&out| graph.value_uses(out).next().is_none())
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

/// `root` plus its transitive input cone, in producer-before-consumer order.
/// Errors on a cycle.
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
