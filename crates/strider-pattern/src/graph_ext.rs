//! Pattern inputs are **sparse**: `call().arg(0, ...)` wires only raw input
//! slot 4. The generic graph stores inputs densely, so each input's original
//! consumer slot rides on the node payload ([`HasInputSlots::input_slots`]),
//! parallel to the generic graph's input order.
//! [`consumed_inputs`] zips the two back together.

use anyhow::anyhow;
use graph_algorithms::walk::{GraphRef, acyclic_postorder};
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

/// A node's consumers: outputs by slot, each output's uses newest first. A
/// node reading one value twice is listed twice, on a sealed graph
/// adjacently, which the walk pops as one.
pub(crate) struct Consumers<'a, N, V>(pub(crate) &'a Graph<N, V, NeverCacheable>);

impl<N, V> GraphRef for Consumers<'_, N, V> {
    type NodeId = NodeId;

    fn successors(&self, node: NodeId, mut f: impl FnMut(NodeId)) {
        for &out in self.0.node_outputs(node) {
            for (consumer, _) in self.0.value_uses(out) {
                f(consumer);
            }
        }
    }
}

/// A node's input producers.
#[cfg(test)]
pub(crate) struct Producers<'a, N, V>(pub(crate) &'a Graph<N, V, NeverCacheable>);

#[cfg(test)]
impl<N, V> GraphRef for Producers<'_, N, V> {
    type NodeId = NodeId;

    fn successors(&self, node: NodeId, mut f: impl FnMut(NodeId)) {
        for value in self.0.node_inputs(node) {
            f(self.0.producer(value));
        }
    }
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

/// Every node, producers first: the reverse of a post-order over consumers
/// from each node in id order. Errors on a cycle anywhere. On a template,
/// whose root is the unique sink and so reached from every node, this order
/// fixes an instantiation's fresh NodeIds.
pub(crate) fn topo_order<N, V>(graph: &Graph<N, V, NeverCacheable>) -> anyhow::Result<Vec<NodeId>> {
    let ids: Vec<NodeId> = graph.all_node_ids().collect();
    // The walk pops its roots last first.
    let mut order = acyclic_postorder(Consumers(graph), ids.into_iter().rev())
        .map_err(|node| anyhow!("pattern graph cycle at {node:?}"))?;
    order.reverse();
    Ok(order)
}

/// `root` plus its transitive input cone, in [`topo_order`]. Errors on a
/// cycle anywhere in the graph.
#[cfg(test)]
pub(crate) fn reachable_topo<N, V>(
    graph: &Graph<N, V, NeverCacheable>,
    root: NodeId,
) -> anyhow::Result<Vec<NodeId>> {
    let order = topo_order(graph)?;
    let mut cone = graph_algorithms::walk::PreOrder::new(Producers(graph), [root]);
    while cone.next().is_some() {}
    let cone = cone.into_visited();
    Ok(order.into_iter().filter(|&n| cone.contains(n)).collect())
}

#[cfg(test)]
mod tests {
    use petgraph::graph::{DiGraph, NodeIndex};
    use rustc_hash::{FxHashMap, FxHashSet};
    use strider_graph::{Graph, NeverCacheable, NodeId, ValueId};
    use strider_ir::node::NodeKind;
    use strider_ir::{IntBinaryOp, IntUnaryOp};
    use strider_ir_test_utils::{Rng, graph_oracle};

    use super::{reachable_topo, topo_order};
    use crate::Capture;
    use crate::template::{
        Template, TemplateBuilder, TemplateKind, TmplNodeRef, TmplValue, TmplValueRef,
    };

    type G = Graph<u8, u8, NeverCacheable>;

    const CASES: u64 = if cfg!(debug_assertions) {
        2_000
    } else {
        50_000
    };

    /// Up to ten nodes, each reading up to two earlier outputs; one graph in
    /// three then has a first input rewired to a node at or after it.
    fn random_graph(rng: &mut Rng) -> (G, Vec<NodeId>) {
        let mut g = G::new();
        let n = 1 + rng.below(10);
        let mut nodes: Vec<NodeId> = Vec::with_capacity(n);
        let mut outs: Vec<ValueId> = Vec::with_capacity(n);
        for i in 0..n {
            let arity = if i == 0 { 0 } else { rng.below(3) };
            let inputs: Vec<ValueId> = (0..arity).map(|_| outs[rng.below(i)]).collect();
            let node = g.create_node(0, inputs, [0]);
            outs.push(g.node_outputs(node)[0]);
            nodes.push(node);
        }
        if rng.below(3) == 0 {
            let i = rng.below(n);
            if let Ok(slot) = g.node_input_id_at(nodes[i], 0) {
                let j = i + rng.below(n - i);
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
        let mut rng = Rng(0x2545_F491_4F6C_DD1D);
        // Acyclic, cyclic in the cone, cyclic only outside it, a cone of five
        // nodes or more, a node reading one producer twice.
        let mut shapes = [0u64; 5];
        for case in 0..CASES {
            let (g, nodes) = random_graph(&mut rng);
            let preds = producers(&g, &nodes);
            // The last node half the time, for cones of more than a few.
            let root = match rng.below(2) {
                0 => nodes.len() as u32 - 1,
                _ => rng.below(nodes.len()) as u32,
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

    /// `root`'s cone in petgraph's `toposort` order over the bipartite graph
    /// (each node, then its outputs, as vertices in id order; a node's edges
    /// to its outputs by slot, an output's to its distinct consumers in
    /// use-list order): the instantiation order these tests pin. Each
    /// vertex's edges go in last to first, as a `DiGraph` lists the newest
    /// edge first.
    fn petgraph_order<N, V>(g: &Graph<N, V, NeverCacheable>, root: NodeId) -> Vec<NodeId> {
        let mut pg: DiGraph<Option<NodeId>, ()> = DiGraph::new();
        let mut node_ix: FxHashMap<NodeId, NodeIndex> = FxHashMap::default();
        let mut value_ix: FxHashMap<ValueId, NodeIndex> = FxHashMap::default();
        for n in g.all_node_ids() {
            node_ix.insert(n, pg.add_node(Some(n)));
            for &out in g.node_outputs(n) {
                value_ix.insert(out, pg.add_node(None));
            }
        }
        for n in g.all_node_ids() {
            for &out in g.node_outputs(n).iter().rev() {
                pg.add_edge(node_ix[&n], value_ix[&out], ());
            }
            for &out in g.node_outputs(n) {
                let mut consumers: Vec<NodeId> = Vec::new();
                for (c, _) in g.value_uses(out) {
                    if !consumers.contains(&c) {
                        consumers.push(c);
                    }
                }
                for &c in consumers.iter().rev() {
                    pg.add_edge(value_ix[&out], node_ix[&c], ());
                }
            }
        }
        let mut cone: FxHashSet<NodeId> = FxHashSet::default();
        let mut stack = vec![root];
        while let Some(n) = stack.pop() {
            if cone.insert(n) {
                stack.extend(g.node_inputs(n).into_iter().map(|v| g.producer(v)));
            }
        }
        let sorted = petgraph::algo::toposort(&pg, None).expect("an acyclic graph");
        sorted
            .into_iter()
            .filter_map(|ix| pg[ix])
            .filter(|n| cone.contains(n))
            .collect()
    }

    /// Whether `t` has a root, and if so that it instantiates in petgraph's
    /// order, which also shows the root's cone is the whole graph.
    fn check_order(t: &Template) -> bool {
        let Ok(root) = t.root() else {
            return false;
        };
        assert_eq!(
            topo_order(&t.graph).unwrap(),
            petgraph_order(&t.graph, root),
            "template instantiation order moved"
        );
        uses_by_one_consumer_sit_together(&t.graph);
        true
    }

    /// Sealing links a node's inputs one after another, so a consumer's uses
    /// of one value are adjacent in its use list. That is why the consumer
    /// walk needs no dedup where petgraph's sort keeps the first of each.
    fn uses_by_one_consumer_sit_together<N, V>(g: &Graph<N, V, NeverCacheable>) {
        for v in g.all_value_ids() {
            let mut users: Vec<NodeId> = g.value_uses(v).map(|(c, _)| c).collect();
            users.dedup();
            let runs = users.len();
            users.sort_by_key(|n| n.as_u32());
            users.dedup();
            assert_eq!(users.len(), runs, "{v:?}'s uses by one consumer are split");
        }
    }

    /// `root`'s cone in its walked order, which is `want` and petgraph's.
    fn check_named(g: &G, root: NodeId, want: &[u32]) {
        let sorted = reachable_topo(g, root).unwrap();
        assert_eq!(petgraph_order(g, root), sorted, "the rebuilt sort");
        let got: Vec<u32> = sorted.iter().map(|n| n.as_u32()).collect();
        assert_eq!(got, want);
    }

    /// Three orders no post-order over input producers from the root gives,
    /// under any order of each node's producers: two leaves under one node,
    /// the right one first; a leaf ordered between two of another node's
    /// cone; a two-output root over a value read twice.
    #[test]
    fn named_graphs_keep_their_instantiation_order() {
        let leaf = |g: &mut G| {
            let n = g.create_node(0, [], [0]);
            g.node_outputs(n)[0]
        };

        let mut g = G::new();
        let (l, r) = (leaf(&mut g), leaf(&mut g));
        let root = g.create_node(0, [l, r], [0]);
        check_named(&g, root, &[1, 0, 2]);

        let mut g = G::new();
        let (v0, v1, v2) = (leaf(&mut g), leaf(&mut g), leaf(&mut g));
        let n3 = g.create_node(0, [v0, v2], [0]);
        let v3 = g.node_outputs(n3)[0];
        let root = g.create_node(0, [v1, v3], [0]);
        check_named(&g, root, &[2, 1, 0, 3, 4]);

        let mut g = G::new();
        let (v0, v1) = (leaf(&mut g), leaf(&mut g));
        let n2 = g.create_node(0, [v0, v1, v1], [0]);
        let n3 = g.create_node(0, [v0, v0], [0]);
        let (v2, v3) = (g.node_outputs(n2)[0], g.node_outputs(n3)[0]);
        let root = g.create_node(0, [v0, v3, v2], [0, 0]);
        check_named(&g, root, &[1, 0, 3, 2, 4]);
    }

    /// A random tree in the builders' two staging orders: operands before the
    /// node (`binary`, `unary`, as the typed unary and int-binary verbs do) or
    /// the node first (`node` then `input`, as the two-input verbs do). Leaves
    /// are built constants or captures.
    fn stage(b: &mut TemplateBuilder, rng: &mut Rng, depth: u32) -> TmplValueRef {
        if depth == 0 || rng.below(3) == 0 {
            return match rng.below(2) {
                0 => b.capture(Capture::new()),
                _ => {
                    let v = strider_ir::UBig::from(rng.below(4));
                    b.dynamic_leaf(TemplateKind::FnIntConst(Box::new(move |_| Ok(v.clone()))))
                }
            };
        }
        match rng.below(3) {
            0 => {
                let x = stage(b, rng, depth - 1);
                b.unary(NodeKind::IntUnaryOp(IntUnaryOp::Neg), x)
            }
            1 => {
                let l = stage(b, rng, depth - 1);
                let r = stage(b, rng, depth - 1);
                b.binary(IntBinaryOp::Add, l, r)
            }
            _ => {
                let n = b.node(NodeKind::IntBinaryOp(IntBinaryOp::Mul));
                let l = stage(b, rng, depth - 1);
                let r = stage(b, rng, depth - 1);
                b.input(n, 0, l);
                b.input(n, 1, r);
                b.value_output(n, 0)
            }
        }
    }

    /// Every tree the builders can stage keeps its instantiation order.
    #[test]
    fn staged_trees_keep_their_instantiation_order() {
        let mut rng = Rng(0x5851_F42D_4C95_7F2D);
        let mut multi_leaf = 0u64;
        for _ in 0..CASES {
            let mut b = TemplateBuilder::new();
            let depth = 1 + rng.below(5) as u32;
            stage(&mut b, &mut rng, depth);
            let t = b.finish();
            multi_leaf += u64::from(t.graph.all_node_ids().count() >= 4);
            assert!(check_order(&t), "a staged tree has one root");
        }
        assert!(
            multi_leaf >= CASES / 4,
            "too few multi-leaf trees: {multi_leaf}"
        );
    }

    /// A raw-builder template of up to ten nodes, created in a random order
    /// under a hidden topological rank: one in five a capture leaf, the rest
    /// with up to three value or memory outputs at random slots and up to
    /// three inputs read off random outputs of lower-ranked nodes, one
    /// consumer in six reading one of its values again at another slot. The
    /// wires go in in a random order, so a node's slots are wired out of
    /// order; `true` beside the template when some node's were.
    ///
    /// Python templates compile through the typed verbs into this same sealed
    /// graph, whose order reads only its structure (creation order, inputs in
    /// wire order, outputs, use lists) and never a kind, so these cover them.
    fn raw_dag(rng: &mut Rng) -> (Template, bool) {
        let mut b = TemplateBuilder::new();
        let n = 1 + rng.below(10);
        let mut rank: Vec<usize> = (0..n).collect();
        for i in (1..n).rev() {
            rank.swap(i, rng.below(i + 1));
        }
        let mut nodes: Vec<Option<TmplNodeRef>> = Vec::with_capacity(n);
        let mut outs: Vec<Vec<TmplValueRef>> = Vec::with_capacity(n);
        for _ in 0..n {
            if rng.below(5) == 0 {
                nodes.push(None);
                outs.push(vec![b.capture(Capture::new())]);
                continue;
            }
            let node = b.node(NodeKind::IntBinaryOp(IntBinaryOp::Add));
            let mut own = Vec::new();
            for _ in 0..rng.below(4) {
                let slot = rng.below(3);
                own.push(match rng.below(4) {
                    0 => b.memory_output(node, slot),
                    _ => b.value_output(node, slot),
                });
            }
            nodes.push(Some(node));
            outs.push(own);
        }
        // Each consumer's reads, as (producer, output) positions.
        let mut reads: Vec<Vec<(usize, usize)>> = vec![Vec::new(); n];
        for c in 0..n {
            if nodes[c].is_none() {
                continue;
            }
            let feeds: Vec<(usize, usize)> = (0..n)
                .filter(|&p| rank[p] < rank[c])
                .flat_map(|p| (0..outs[p].len()).map(move |k| (p, k)))
                .collect();
            if feeds.is_empty() {
                continue;
            }
            for _ in 0..rng.below(4) {
                reads[c].push(feeds[rng.below(feeds.len())]);
            }
        }
        // A node no one reads is a second sink: one higher-ranked built node
        // reads it, so most templates keep a single root.
        for p in 0..n {
            let read = reads.iter().flatten().any(|&(q, _)| q == p);
            let above: Vec<usize> = (0..n)
                .filter(|&c| rank[c] > rank[p] && nodes[c].is_some())
                .collect();
            if !read && !outs[p].is_empty() && !above.is_empty() {
                let k = rng.below(outs[p].len());
                reads[above[rng.below(above.len())]].push((p, k));
            }
        }
        let mut wires: Vec<(usize, usize, TmplValueRef)> = Vec::new();
        for (c, mut read) in reads.into_iter().enumerate() {
            if !read.is_empty() && rng.below(6) == 0 {
                read.push(read[rng.below(read.len())]);
            }
            for (slot, (p, k)) in read.into_iter().enumerate() {
                wires.push((c, slot, outs[p][k]));
            }
        }
        for i in (1..wires.len()).rev() {
            wires.swap(i, rng.below(i + 1));
        }
        let mut last_slot: Vec<Option<usize>> = vec![None; n];
        let mut out_of_order = false;
        for (c, slot, v) in wires {
            out_of_order |= last_slot[c].is_some_and(|s| s > slot);
            last_slot[c] = Some(slot);
            b.input(nodes[c].expect("a consumer is a built node"), slot, v);
        }
        (b.finish(), out_of_order)
    }

    /// Every raw-builder DAG with a root keeps its instantiation order.
    #[test]
    fn raw_dags_keep_their_instantiation_order() {
        let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
        // Rooted; DAG-shaped (a value read twice or a node with two read
        // outputs); one value read by two nodes; one value read twice by one
        // node; two read outputs of one node; slots wired out of order; a read
        // memory output.
        let mut shapes = [0u64; 7];
        for _ in 0..CASES {
            let (t, out_of_order) = raw_dag(&mut rng);
            if !check_order(&t) {
                continue;
            }
            let g = &t.graph;
            let read_twice = g.all_value_ids().any(|v| g.value_uses(v).count() >= 2);
            let shared = g.all_value_ids().any(|v| {
                let mut readers = g.value_uses(v).map(|(n, _)| n);
                readers
                    .next()
                    .is_some_and(|first| readers.any(|n| n != first))
            });
            let two_read_outputs = g.all_node_ids().any(|n| {
                let outs = g.node_outputs(n);
                outs.iter()
                    .filter(|&&o| g.value_uses(o).next().is_some())
                    .count()
                    >= 2
            });
            let double_read = g.all_node_ids().any(|n| {
                let ins: Vec<ValueId> = g.node_inputs(n).into_iter().collect();
                (1..ins.len()).any(|i| ins[..i].contains(&ins[i]))
            });
            let memory_read = g.all_value_ids().any(|v| {
                matches!(g.value_kind_ref(v), TmplValue::Memory { .. })
                    && g.value_uses(v).next().is_some()
            });
            shapes[0] += 1;
            shapes[1] += u64::from(read_twice || two_read_outputs);
            shapes[2] += u64::from(shared);
            shapes[3] += u64::from(double_read);
            shapes[4] += u64::from(two_read_outputs);
            shapes[5] += u64::from(out_of_order);
            shapes[6] += u64::from(memory_read);
        }
        let kept = shapes[0];
        assert!(kept >= CASES / 4, "too few rooted templates: {shapes:?}");
        assert!(shapes[1] >= kept / 4, "too few DAGs: {shapes:?}");
        assert!(
            shapes[2..].iter().all(|&c| c >= kept / 80),
            "too few of a shape, floor {}: {shapes:?}",
            kept / 80
        );
    }
}
