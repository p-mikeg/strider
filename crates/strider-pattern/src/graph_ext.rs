//! Pattern inputs are sparse (`call().arg(0, ...)` wires only raw slot 4) while the graph stores
//! them densely, so each input's slot rides on the node payload
//! ([`HasInputSlots::input_slots`]).

use anyhow::anyhow;
use strider_graph::{Graph, NeverCacheable, NodeId, ValueId};

pub(crate) trait HasInputSlots {
    /// An index, or on the match side possibly an existential.
    type Slot: Copy;
    /// Consumer slots, parallel to the graph's input order.
    fn input_slots(&self) -> &[Self::Slot];
}

/// Each input with its consumer slot.
pub(crate) fn consumed_inputs<N: HasInputSlots, V>(
    graph: &Graph<N, V, NeverCacheable>,
    node: NodeId,
) -> Vec<(N::Slot, ValueId)> {
    let slots = graph.node_kind(node).input_slots();
    graph
        .node_inputs(node)
        .into_iter()
        .enumerate()
        .map(|(i, value)| (slots[i], value))
        .collect()
}

/// The root: the unique sink. A node with no outputs is a sink.
///
/// # Errors
/// Unless there is exactly one sink.
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
        [] => Err(anyhow!("pattern graph has no sink node (it is empty)")),
        many => Err(anyhow!(
            "pattern graph has {} sink nodes; expected exactly one (multi-rooted)",
            many.len()
        )),
    }
}
