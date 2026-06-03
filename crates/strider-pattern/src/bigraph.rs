//! A generic bipartite `Node → Output → Node` graph.
//!
//! [`BiGraph`] owns the graph mechanics shared by the match side
//! ([`Pattern`](crate::matcher::Pattern)) and the build side: two vertex
//! kinds ([`BiVertex::Node`] / [`BiVertex::Output`]), two edge kinds
//! ([`BiEdge::Produces`] node → its output, [`BiEdge::Consumes`] output →
//! a consuming node at a slot), and the reachable-topological ordering.
//! It is parameterised over the node payload `N` and the output payload
//! `O`, so a caller instantiates it with its own match / build payloads
//! and reaches the structure only through the accessors below — never
//! into `petgraph` directly.

use anyhow::anyhow;
use petgraph::stable_graph::{NodeIndex, StableDiGraph};
use petgraph::visit::{DfsPostOrder, EdgeRef, Reversed, Walker};

/// A vertex in the bipartite graph: either a node or one of a node's
/// outputs.
pub enum BiVertex<N, O> {
    /// A node-shaped vertex carrying the node payload.
    Node(N),
    /// An output-shaped vertex carrying the output payload.
    Output(O),
}

/// A directed edge in the bipartite graph.
///
/// `Produces` runs from a producer node vertex to one of its output
/// vertices; `Consumes` runs from an output vertex to a consuming node
/// vertex, recording which input slot of the consumer the output feeds.
#[derive(Clone, Copy, Debug)]
pub enum BiEdge {
    /// Producer node → its output vertex.
    Produces,
    /// Output vertex → consuming node at the given input slot.
    Consumes { slot: usize },
}

/// A generic bipartite directed graph with node and output vertices.
///
/// The graph stores no root: the structural root is the unique sink node,
/// recovered on demand via [`derive_root`](Self::derive_root).
pub struct BiGraph<N, O> {
    inner: StableDiGraph<BiVertex<N, O>, BiEdge>,
}

impl<N, O> Default for BiGraph<N, O> {
    fn default() -> Self {
        Self::new()
    }
}

impl<N, O> BiGraph<N, O> {
    /// An empty graph.
    pub fn new() -> Self {
        Self {
            inner: StableDiGraph::new(),
        }
    }

    // ── construction ────────────────────────────────────────────────

    /// Adds a node vertex, returning its index.
    pub fn add_node(&mut self, n: N) -> NodeIndex {
        self.inner.add_node(BiVertex::Node(n))
    }

    /// Adds an output vertex produced by `producer`, wiring the
    /// `Produces` edge, returning its index.
    pub fn add_output(&mut self, producer: NodeIndex, o: O) -> NodeIndex {
        let idx = self.inner.add_node(BiVertex::Output(o));
        self.inner.add_edge(producer, idx, BiEdge::Produces);
        idx
    }

    /// Wires `output` into `consumer`'s input `slot` (a `Consumes` edge).
    pub fn consume(&mut self, consumer: NodeIndex, slot: usize, output: NodeIndex) {
        self.inner.add_edge(output, consumer, BiEdge::Consumes { slot });
    }

    /// The unique **sink** node — a node vertex none of whose produced
    /// outputs are consumed — recovered structurally rather than stored.
    ///
    /// In a single-rooted pattern / template every interior node feeds the
    /// root, so it has at least one consumed output; only the root has no
    /// consumed output. (A multi-output interior node such as a `Call` with
    /// a dangling memory token still has its value output consumed, so it is
    /// not a sink — "all outputs unconsumed" is the test, not "some".)
    ///
    /// # Errors
    /// Errors unless there is exactly one sink: zero means the graph is
    /// rootless or cyclic; more than one means it is multi-rooted (raised
    /// rather than guessed).
    pub fn derive_root(&self) -> anyhow::Result<NodeIndex> {
        let sinks: Vec<NodeIndex> = self
            .inner
            .node_indices()
            .filter(|&idx| {
                matches!(self.inner.node_weight(idx), Some(BiVertex::Node(_)))
                    && self
                        .produced_outputs(idx)
                        .all(|out| !self.output_is_consumed(out))
            })
            .collect();
        match sinks.as_slice() {
            [only] => Ok(*only),
            [] => Err(anyhow!("BiGraph has no sink node (rootless or cyclic)")),
            many => Err(anyhow!(
                "BiGraph has {} sink nodes; expected exactly one (multi-rooted)",
                many.len()
            )),
        }
    }

    /// Whether output vertex `output` feeds at least one consumer (has an
    /// outgoing `Consumes` edge).
    fn output_is_consumed(&self, output: NodeIndex) -> bool {
        self.inner
            .edges_directed(output, petgraph::Outgoing)
            .any(|e| matches!(e.weight(), BiEdge::Consumes { .. }))
    }

    // ── counts (test / invariant helpers) ───────────────────────────

    /// Number of node vertices.
    pub fn node_count(&self) -> usize {
        self.inner
            .node_weights()
            .filter(|v| matches!(v, BiVertex::Node(_)))
            .count()
    }

    /// Number of output vertices.
    pub fn output_count(&self) -> usize {
        self.inner
            .node_weights()
            .filter(|v| matches!(v, BiVertex::Output(_)))
            .count()
    }

    // ── vertex-weight accessors ─────────────────────────────────────

    /// The node payload at `idx`, or `None` if `idx` is missing or an
    /// output vertex.
    pub fn node_weight(&self, idx: NodeIndex) -> Option<&N> {
        match self.inner.node_weight(idx) {
            Some(BiVertex::Node(n)) => Some(n),
            _ => None,
        }
    }

    /// Mutable access to the node payload at `idx`.
    pub fn node_weight_mut(&mut self, idx: NodeIndex) -> Option<&mut N> {
        match self.inner.node_weight_mut(idx) {
            Some(BiVertex::Node(n)) => Some(n),
            _ => None,
        }
    }

    /// The output payload at `idx`, or `None` if `idx` is missing or a
    /// node vertex.
    pub fn output_weight(&self, idx: NodeIndex) -> Option<&O> {
        match self.inner.node_weight(idx) {
            Some(BiVertex::Output(o)) => Some(o),
            _ => None,
        }
    }

    /// Mutable access to the output payload at `idx`.
    pub fn output_weight_mut(&mut self, idx: NodeIndex) -> Option<&mut O> {
        match self.inner.node_weight_mut(idx) {
            Some(BiVertex::Output(o)) => Some(o),
            _ => None,
        }
    }

    /// Every node payload, for whole-graph scans (counts / capture
    /// coverage).
    pub fn node_weights(&self) -> impl Iterator<Item = &N> {
        self.inner.node_weights().filter_map(|v| match v {
            BiVertex::Node(n) => Some(n),
            BiVertex::Output(_) => None,
        })
    }

    /// Every output payload, for whole-graph scans.
    pub fn output_weights(&self) -> impl Iterator<Item = &O> {
        self.inner.node_weights().filter_map(|v| match v {
            BiVertex::Output(o) => Some(o),
            BiVertex::Node(_) => None,
        })
    }

    // ── edge navigation ─────────────────────────────────────────────

    /// The node vertex that produces output vertex `output` (the source
    /// of its incoming `Produces` edge), or `None` if the output vertex
    /// has no producer (a malformed graph).
    pub fn producer_of(&self, output: NodeIndex) -> Option<NodeIndex> {
        self.inner
            .edges_directed(output, petgraph::Incoming)
            .find(|e| matches!(e.weight(), BiEdge::Produces))
            .map(|e| e.source())
    }

    /// The `Consumes` inputs into `node`: each yielded `(slot,
    /// output_idx)` is an output vertex feeding `node`'s input `slot`.
    pub fn consumed_inputs(&self, node: NodeIndex) -> impl Iterator<Item = (usize, NodeIndex)> + '_ {
        self.inner
            .edges_directed(node, petgraph::Incoming)
            .filter_map(|e| match e.weight() {
                BiEdge::Consumes { slot } => Some((*slot, e.source())),
                BiEdge::Produces => None,
            })
    }

    /// The output vertices `node` produces (targets of its outgoing
    /// `Produces` edges).
    pub fn produced_outputs(&self, node: NodeIndex) -> impl Iterator<Item = NodeIndex> + '_ {
        self.inner
            .edges_directed(node, petgraph::Outgoing)
            .filter_map(|e| match e.weight() {
                BiEdge::Produces => Some(e.target()),
                BiEdge::Consumes { .. } => None,
            })
    }
}

/// Returns every vertex reachable backwards from `root` (i.e. `root`
/// and its transitive input cone) in producer-before-consumer
/// topological order.
///
/// Reachability follows reversed edges from `root` (each consumer
/// reaches its producers); the global toposort is then filtered to the
/// reachable set, preserving the topological order. Errors if the graph
/// contains a cycle.
pub(crate) fn reachable_topo<N, O>(
    g: &BiGraph<N, O>,
    root: NodeIndex,
) -> anyhow::Result<Vec<NodeIndex>> {
    let reachable: std::collections::HashSet<NodeIndex> = DfsPostOrder::new(Reversed(&g.inner), root)
        .iter(Reversed(&g.inner))
        .collect();
    let sorted = petgraph::algo::toposort(&g.inner, None)
        .map_err(|c| anyhow!("BiGraph cycle at {:?}", c.node_id()))?;
    Ok(sorted.into_iter().filter(|n| reachable.contains(n)).collect())
}

/// Asserts that the graph is acyclic by running [`reachable_topo`] and
/// discarding the order.
pub(crate) fn assert_dag<N, O>(g: &BiGraph<N, O>, root: NodeIndex) -> anyhow::Result<()> {
    reachable_topo(g, root).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A small bipartite shape over a dummy `(N = &str, O = u32)`
    /// payload: two leaf nodes each producing an output, both consumed
    /// by a sink node, which produces its own output.
    ///
    /// ```text
    ///   "x" ──Produces──▶ out(10) ──Consumes{0}──▶ "sum"
    ///   "k" ──Produces──▶ out(11) ──Consumes{1}──▶ "sum" ──Produces──▶ out(12)
    /// ```
    #[test]
    fn accessors_and_topo_over_dummy_payload() {
        let mut g: BiGraph<&str, u32> = BiGraph::new();
        let x = g.add_node("x");
        let xout = g.add_output(x, 10);
        let k = g.add_node("k");
        let kout = g.add_output(k, 11);
        let sum = g.add_node("sum");
        g.consume(sum, 0, xout);
        g.consume(sum, 1, kout);
        let sumout = g.add_output(sum, 12);

        assert_eq!(g.node_count(), 3);
        assert_eq!(g.output_count(), 3);
        // The root is recovered structurally — sum is the unique sink.
        assert_eq!(g.derive_root().unwrap(), sum);

        // Weight accessors discriminate node vs output vertices.
        assert_eq!(g.node_weight(x), Some(&"x"));
        assert_eq!(g.node_weight(xout), None);
        assert_eq!(g.output_weight(xout), Some(&10));
        assert_eq!(g.output_weight(x), None);

        // producer_of walks the Produces edge backwards.
        assert_eq!(g.producer_of(xout), Some(x));
        assert_eq!(g.producer_of(kout), Some(k));
        assert_eq!(g.producer_of(sumout), Some(sum));

        // consumed_inputs yields (slot, output_vertex) for the sink.
        let mut inputs: Vec<(usize, NodeIndex)> = g.consumed_inputs(sum).collect();
        inputs.sort_by_key(|&(slot, _)| slot);
        assert_eq!(inputs, vec![(0, xout), (1, kout)]);
        assert_eq!(g.consumed_inputs(x).count(), 0);

        // produced_outputs yields the node's output vertices.
        assert_eq!(g.produced_outputs(x).collect::<Vec<_>>(), vec![xout]);
        assert_eq!(g.produced_outputs(sum).collect::<Vec<_>>(), vec![sumout]);

        // The *_weights iterators scan every vertex of each kind.
        assert_eq!(g.node_weights().count(), 3);
        assert_eq!(g.output_weights().count(), 3);

        // Mut accessors round-trip: mutate a payload, read it back. The
        // mut accessor also discriminates vertex kind (node-mut on an
        // output vertex is None and vice versa).
        *g.node_weight_mut(x).unwrap() = "x2";
        assert_eq!(g.node_weight(x), Some(&"x2"));
        assert!(g.node_weight_mut(xout).is_none());
        *g.output_weight_mut(xout).unwrap() = 99;
        assert_eq!(g.output_weight(xout), Some(&99));
        assert!(g.output_weight_mut(x).is_none());

        // reachable_topo orders producers before the consumer.
        let order = reachable_topo(&g, g.derive_root().unwrap()).unwrap();
        let pos = |n: NodeIndex| order.iter().position(|&m| m == n).unwrap();
        assert!(pos(x) < pos(sum));
        assert!(pos(k) < pos(sum));
        assert_dag(&g, g.derive_root().unwrap()).unwrap();
    }

    /// `reachable_topo` / `assert_dag` must reject a cyclic graph. A
    /// cycle is constructible through the safe API alone: have node `a`
    /// produce `aout`, node `b` consume `aout` and produce `bout`, then
    /// have `a` consume `bout` — yielding the cycle
    /// `a ─▶ aout ─▶ b ─▶ bout ─▶ a` over alternating Produces/Consumes
    /// edges.
    #[test]
    fn reachable_topo_rejects_cycle() {
        let mut g: BiGraph<&str, u32> = BiGraph::new();
        let a = g.add_node("a");
        let aout = g.add_output(a, 1);
        let b = g.add_node("b");
        let bout = g.add_output(b, 2);
        g.consume(b, 0, aout);
        g.consume(a, 0, bout);

        assert!(reachable_topo(&g, a).is_err());
        assert!(assert_dag(&g, a).is_err());
        // A cyclic graph has no sink, so derive_root errors too.
        assert!(g.derive_root().is_err());
    }

    /// `derive_root` recovers the root structurally: the unique node none
    /// of whose outputs are consumed (the DAG sink).
    #[test]
    fn derive_root_finds_unique_sink() {
        let mut g: BiGraph<&str, u32> = BiGraph::new();
        let x = g.add_node("x");
        let xout = g.add_output(x, 10);
        let sum = g.add_node("sum");
        g.consume(sum, 0, xout);
        let _sumout = g.add_output(sum, 12);
        // x's output is consumed by sum; sum's output is consumed by
        // nobody — sum is the unique sink.
        assert_eq!(g.derive_root().unwrap(), sum);
    }

    /// Two unconnected nodes each with an unconsumed output are two sinks;
    /// `derive_root` refuses to guess.
    #[test]
    fn derive_root_rejects_two_sinks() {
        let mut g: BiGraph<&str, u32> = BiGraph::new();
        let a = g.add_node("a");
        let _aout = g.add_output(a, 1);
        let b = g.add_node("b");
        let _bout = g.add_output(b, 2);
        assert!(g.derive_root().is_err());
    }

    /// A node with one consumed and one unconsumed output is still a sink
    /// (only "all outputs unconsumed" disqualifies a node, not "some").
    /// Here the producer's value feeds the sink while the producer's second
    /// output dangles — the producer must not be mistaken for a sink.
    #[test]
    fn derive_root_ignores_partially_consumed_interior_node() {
        let mut g: BiGraph<&str, u32> = BiGraph::new();
        let producer = g.add_node("call");
        let val = g.add_output(producer, 0); // consumed
        let _mem = g.add_output(producer, 1); // dangles
        let sink = g.add_node("ret");
        g.consume(sink, 0, val);
        let _sinkout = g.add_output(sink, 9);
        assert_eq!(g.derive_root().unwrap(), sink);
    }
}
