//! Pattern inputs are **sparse**: `call().arg(0, ...)` wires only raw input
//! slot 4. The generic graph stores inputs densely, so each input's original
//! consumer slot rides on the node payload ([`HasInputSlots::input_slots`]),
//! parallel to the generic graph's input order.
//! [`consumed_inputs`] zips the two back together.

use anyhow::anyhow;
use strider_graph::{Graph, NeverCacheable, NodeId, ValueId};

pub(crate) trait HasInputSlots {
    /// A plain index on the build side; on the match side an existential
    /// names no index.
    type Slot: Copy;
    /// Consumer slots, one per input, parallel to the generic graph's input
    /// order.
    fn input_slots(&self) -> &[Self::Slot];
}

/// Recovers each input's sparse consumer slot from the node payload.
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

/// The root is the unique sink, derived structurally rather than stored. A
/// node with no outputs is vacuously a sink, which is what roots `ret()` /
/// `switch()` / `if_else()`.
///
/// # Errors
/// Unless there is exactly one sink: zero means an empty graph, more than one
/// means multi-rooted.
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
