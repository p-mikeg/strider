//! Reachable-topological ordering of the bipartite pattern graph via
//! petgraph's library algorithms.

use anyhow::anyhow;
use petgraph::stable_graph::{NodeIndex, StableDiGraph};
use petgraph::visit::{DfsPostOrder, Reversed, Walker};

use crate::pattern::{PatEdge, PatVertex};

/// Returns every vertex reachable backwards from `root` (i.e. `root`
/// and its transitive input cone) in producer-before-consumer
/// topological order.
///
/// Reachability follows reversed edges from `root` (each consumer
/// reaches its producers); the global toposort is then filtered to the
/// reachable set, preserving the topological order. Errors if the
/// graph contains a cycle.
pub(crate) fn reachable_topo(
    g: &StableDiGraph<PatVertex, PatEdge>,
    root: NodeIndex,
) -> anyhow::Result<Vec<NodeIndex>> {
    let reachable: std::collections::HashSet<NodeIndex> =
        DfsPostOrder::new(Reversed(g), root).iter(Reversed(g)).collect();
    let sorted = petgraph::algo::toposort(g, None)
        .map_err(|c| anyhow!("PatGraph cycle at {:?}", c.node_id()))?;
    Ok(sorted.into_iter().filter(|n| reachable.contains(n)).collect())
}

/// Asserts that the pattern graph is acyclic.
///
/// Cycle detection runs over the whole graph (`toposort(g, None)`), not
/// just the cone reachable from `root`.
pub(crate) fn assert_dag(
    g: &StableDiGraph<PatVertex, PatEdge>,
    root: NodeIndex,
) -> anyhow::Result<()> {
    reachable_topo(g, root).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pattern::{PatNode, PatOutput, Pattern};

    #[test]
    fn reachable_topo_orders_producers_before_consumers() {
        let mut p = Pattern::new();
        let a = p.add_node(PatNode::wildcard());
        let ao = p.add_output(a, PatOutput::value(0));
        let b = p.add_node(PatNode::wildcard());
        p.consume(b, 0, ao);
        p.set_root(b);
        let order = reachable_topo(&p.inner, p.root.unwrap()).unwrap();
        let pa = order.iter().position(|&n| n == a).unwrap();
        let pb = order.iter().position(|&n| n == b).unwrap();
        assert!(pa < pb);
    }
}
