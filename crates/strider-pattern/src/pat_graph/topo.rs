//! Topological iteration over a `PatGraph` rooted at `root`.
//!
//! The user's contract guarantees DAG (no cycles); we assert it at
//! pattern-finalisation time so a builder bug surfaces immediately.

use anyhow::anyhow;
use petgraph::stable_graph::{NodeIndex, StableDiGraph};
use petgraph::visit::EdgeRef;

use super::node_data::{EdgeData, NodeData};

/// Returns nodes reachable from `root` in reverse topological order
/// (leaves first, root last).  Errors if a cycle is detected.
// `dead_code` allow: wired in upcoming matcher / `into_pat` tasks.
// (Tests exercise it directly.)
#[allow(dead_code)]
pub(crate) fn topo_order_from_root(
    g: &StableDiGraph<NodeData, EdgeData>,
    root: NodeIndex,
) -> anyhow::Result<Vec<NodeIndex>> {
    let mut order = Vec::new();
    let mut visited = std::collections::HashSet::<NodeIndex>::new();
    let mut on_stack = std::collections::HashSet::<NodeIndex>::new();
    let mut stack: Vec<(NodeIndex, bool)> = vec![(root, false)];
    while let Some((n, expanded)) = stack.pop() {
        if expanded {
            on_stack.remove(&n);
            order.push(n);
            continue;
        }
        if !visited.insert(n) {
            continue;
        }
        on_stack.insert(n);
        stack.push((n, true));
        // Pattern edges go producer → consumer; iterate incoming edges
        // (producers feeding `n`) to schedule them before `n`.
        for edge in g.edges_directed(n, petgraph::Incoming) {
            let producer = edge.source();
            if on_stack.contains(&producer) {
                return Err(anyhow!(
                    "PatGraph cycle detected: {producer:?} reachable from itself",
                ));
            }
            stack.push((producer, false));
        }
    }
    Ok(order)
}

/// Asserts that the graph rooted at `root` is a DAG.  Used at
/// pattern-finalisation (`into_pat`) time.
// `dead_code` allow: wired in the upcoming `into_pat` finalisation task.
#[allow(dead_code)]
pub(crate) fn assert_dag(
    g: &StableDiGraph<NodeData, EdgeData>,
    root: NodeIndex,
) -> anyhow::Result<()> {
    topo_order_from_root(g, root).map(|_| ())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::pat_graph::node_data::{EdgeData, KindSpec, NodeData};
    use crate::pat_graph::{Concrete, PatGraph};
    use strider_ir::node::NodeOutputType;

    fn dummy_node() -> NodeData {
        NodeData {
            kind: KindSpec::Any,
            output_ty: Some(NodeOutputType::I64),
            capture: None,
            post_match: None,
            build_spec: None,
            force_ordered: false,
        }
    }

    #[test]
    fn topo_chain_returns_leaves_first() {
        let mut g: PatGraph<Concrete> = PatGraph::new();
        let a = g.add_node(dummy_node());
        let b = g.add_node(dummy_node());
        let c = g.add_node(dummy_node());
        g.add_edge(
            a,
            b,
            EdgeData {
                consumer_slot: 0,
                producer_output_slot: 0,
            },
        );
        g.add_edge(
            b,
            c,
            EdgeData {
                consumer_slot: 0,
                producer_output_slot: 0,
            },
        );
        let order = topo_order_from_root(&g.inner, c).unwrap();
        assert_eq!(order, vec![a, b, c]);
    }

    #[test]
    fn topo_detects_cycle() {
        let mut g: PatGraph<Concrete> = PatGraph::new();
        let a = g.add_node(dummy_node());
        let b = g.add_node(dummy_node());
        g.add_edge(
            a,
            b,
            EdgeData {
                consumer_slot: 0,
                producer_output_slot: 0,
            },
        );
        g.add_edge(
            b,
            a,
            EdgeData {
                consumer_slot: 0,
                producer_output_slot: 0,
            },
        );
        let err = topo_order_from_root(&g.inner, a).unwrap_err();
        assert!(err.to_string().contains("cycle"));
    }
}
