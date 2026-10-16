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

/// The root is the unique sink, derived structurally rather than stored. A
/// node with no outputs is vacuously a sink, which is what roots `ret()` /
/// `switch()` / `if_else()`.
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

#[cfg(test)]
mod tests {
    use strider_graph::{Graph, NeverCacheable, NodeId, ValueId};
    use strider_ir_test_utils::graph_oracle;

    use super::reachable_topo;

    type G = Graph<u8, u8, NeverCacheable>;

    const CASES: u64 = if cfg!(debug_assertions) {
        2_000
    } else {
        50_000
    };

    fn next(state: &mut u64, bound: u64) -> u64 {
        *state ^= *state << 13;
        *state ^= *state >> 7;
        *state ^= *state << 17;
        *state % bound
    }

    /// Up to ten nodes, each reading up to two earlier outputs; one graph in
    /// three then has a first input rewired to a node at or after it.
    fn random_graph(state: &mut u64) -> (G, Vec<NodeId>) {
        let mut g = G::new();
        let n = 1 + next(state, 10) as usize;
        let mut nodes: Vec<NodeId> = Vec::with_capacity(n);
        let mut outs: Vec<ValueId> = Vec::with_capacity(n);
        for i in 0..n {
            let arity = if i == 0 { 0 } else { next(state, 3) };
            let inputs: Vec<ValueId> = (0..arity)
                .map(|_| outs[next(state, i as u64) as usize])
                .collect();
            let node = g.create_node(0, inputs, [0]);
            outs.push(g.node_outputs(node)[0]);
            nodes.push(node);
        }
        if next(state, 3) == 0 {
            let i = next(state, n as u64) as usize;
            if let Ok(slot) = g.node_input_id_at(nodes[i], 0) {
                let j = i + next(state, (n - i) as u64) as usize;
                g.update_input(slot, outs[j]);
            }
        }
        (g, nodes)
    }

    /// Each node's producers, by index into `nodes`.
    fn producers(g: &G, nodes: &[NodeId]) -> Vec<Vec<u32>> {
        nodes
            .iter()
            .map(|&v| {
                g.node_inputs(v)
                    .into_iter()
                    .map(|x| {
                        let p = g.producer(x);
                        nodes
                            .iter()
                            .position(|&m| m == p)
                            .expect("a generated producer") as u32
                    })
                    .collect()
            })
            .collect()
    }

    /// `Ok` exactly on an acyclic graph, holding the root's input cone once
    /// each, producers first. A cycle anywhere refuses the order, the cone's
    /// own or not.
    #[test]
    fn reachable_topo_matches_a_brute_force_cone() {
        let mut state = 0x2545_F491_4F6C_DD1D;
        // Acyclic, cyclic in the cone, cyclic only outside it, a cone of five
        // nodes or more, a node reading one producer twice.
        let mut shapes = [0u64; 5];
        for case in 0..CASES {
            let (g, nodes) = random_graph(&mut state);
            let preds = producers(&g, &nodes);
            // The last node half the time, for cones of more than a few.
            let root = match next(&mut state, 2) {
                0 => nodes.len() as u32 - 1,
                _ => next(&mut state, nodes.len() as u64) as u32,
            };
            let cone = graph_oracle::reachable(&preds, &[root], None);
            let on_cycle: Vec<bool> = (0..nodes.len() as u32)
                .map(|v| graph_oracle::reaches(&preds, v, v))
                .collect();
            let cone_cyclic = (0..nodes.len()).any(|v| cone[v] && on_cycle[v]);
            let cyclic = on_cycle.contains(&true);
            shapes[0] += u64::from(!cyclic);
            shapes[1] += u64::from(cone_cyclic);
            shapes[2] += u64::from(cyclic && !cone_cyclic);
            shapes[3] += u64::from(cone.iter().filter(|&&c| c).count() >= 5);
            shapes[4] += u64::from(preds.iter().any(|ps| ps.len() == 2 && ps[0] == ps[1]));
            match reachable_topo(&g, nodes[root as usize]) {
                Err(_) => assert!(cyclic, "case {case}: Err on an acyclic graph"),
                Ok(order) => {
                    assert!(!cyclic, "case {case}: Ok on a cyclic graph");
                    let mut pos = vec![usize::MAX; nodes.len()];
                    for (i, v) in order.iter().enumerate() {
                        let at = nodes.iter().position(|m| m == v).expect("a generated node");
                        assert_eq!(pos[at], usize::MAX, "case {case}: {at} twice");
                        pos[at] = i;
                    }
                    for (v, &inside) in cone.iter().enumerate() {
                        assert_eq!(pos[v] != usize::MAX, inside, "case {case}: node {v}");
                    }
                    for (v, ps) in preds.iter().enumerate().filter(|&(v, _)| cone[v]) {
                        for &p in ps {
                            assert!(pos[p as usize] < pos[v], "case {case}: {p} after {v}");
                        }
                    }
                }
            }
        }
        assert!(
            shapes.iter().all(|&c| c >= CASES / 80),
            "too few of a shape, floor {}: {shapes:?}",
            CASES / 80
        );
    }

    /// `a -> b -> c` with `b` rewired to read `c`: a cycle in `c`'s cone.
    #[test]
    fn a_cycle_in_the_cone_is_an_error() {
        let mut g = G::new();
        let a = g.create_node(0, [], [0]);
        let av = g.node_outputs(a)[0];
        let b = g.create_node(0, [av], [0]);
        let bv = g.node_outputs(b)[0];
        let c = g.create_node(0, [bv], [0]);
        let cv = g.node_outputs(c)[0];
        let slot = g.node_input_id_at(b, 0).unwrap();
        g.update_input(slot, cv);
        assert!(reachable_topo(&g, c).is_err());
    }

    /// The root's cone is acyclic, but the order is a whole-graph topological
    /// sort, so a cycle elsewhere still refuses it.
    #[test]
    fn a_cycle_outside_the_cone_is_an_error() {
        let mut g = G::new();
        let root = g.create_node(0, [], [0]);
        let rv = g.node_outputs(root)[0];
        let x = g.create_node(0, [rv], [0]);
        let xv = g.node_outputs(x)[0];
        let y = g.create_node(0, [xv], [0]);
        let yv = g.node_outputs(y)[0];
        let slot = g.node_input_id_at(x, 0).unwrap();
        g.update_input(slot, yv);
        assert!(reachable_topo(&g, root).is_err());
    }

    #[test]
    #[cfg_attr(debug_assertions, ignore = "release-only scale test")]
    fn reachable_topo_orders_a_hundred_thousand_node_chain() {
        let mut g = G::new();
        let mut prev = g.create_node(0, [], [0]);
        for _ in 1..100_000 {
            let v = g.node_outputs(prev)[0];
            prev = g.create_node(0, [v], [0]);
        }
        assert_eq!(reachable_topo(&g, prev).unwrap().len(), 100_000);
    }
}
