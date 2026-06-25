//! Generic staging core shared by the match- and build-side builders.
//!
//! Both [`MatcherBuilder`](crate::matcher::MatcherBuilder) and
//! [`TemplateBuilder`](crate::template::TemplateBuilder) build a bipartite
//! pattern DAG incrementally — a bare node is staged, its value outputs are
//! added later, and its inputs are wired after the producers exist — and then
//! materialise the whole DAG into a [`strider_graph::Graph`] in
//! producer-before-consumer order. That staging + topological seal is
//! identical on both sides modulo the node / output payload types, so it
//! lives here once. The two builders embed a [`StagedGraph`] and add only
//! their side-specific verbs and annotators on top, which is what keeps the
//! match / template split (and its compile-time wildcard-in-RHS guard) intact:
//! the two remain distinct types.
//!
//! The staging store **is** a petgraph [`DiGraph`] — one petgraph node per
//! staged builder node, one edge per producer→consumer dependency — so the
//! producer-before-consumer order is `petgraph::algo::toposort`, not a
//! hand-rolled sort. Input *order* (and the sparse consumer slot of each
//! input) rides on the node weight, since petgraph edge iteration order is
//! unspecified.

use anyhow::anyhow;
use petgraph::{
    algo::toposort,
    graph::{DiGraph, NodeIndex},
};
use strider_graph::{Graph, NeverCacheable, ValueId};

/// A node staged for materialisation: its payload, the output payloads it
/// produces (in slot-add order), and its `(consumer_slot, producer_node,
/// producer_output)` inputs (in wire order).
struct StagedNode<N, V> {
    kind: N,
    outputs: Vec<V>,
    inputs: Vec<(usize, usize, usize)>,
}

/// Bridges a staged node payload `N` to the sealed-graph payload, stamping
/// the recovered sparse consumer-slot list onto it.
///
/// The match side seals `PatNode → PatNode` (filling its `input_slots`
/// field); the build side seals `TmplNodeKind → TmplNode { kind, input_slots }`.
pub(crate) trait SealNode {
    /// The payload type stored in the sealed [`strider_graph::Graph`].
    type Sealed;
    /// Consume the staged payload, stamping `input_slots`, into the sealed
    /// payload.
    fn seal(self, input_slots: Vec<usize>) -> Self::Sealed;
}

/// The shared staging store + topological seal.
///
/// Node identity is exposed as a plain `usize` (a petgraph
/// [`NodeIndex`]'s `.index()`); staging never removes nodes, so indices stay
/// contiguous and the two are interchangeable.
pub(crate) struct StagedGraph<N, V> {
    g: DiGraph<StagedNode<N, V>, ()>,
}

impl<N, V> Default for StagedGraph<N, V> {
    fn default() -> Self {
        Self::new()
    }
}

impl<N, V> StagedGraph<N, V> {
    /// An empty staging store.
    pub(crate) fn new() -> Self {
        Self { g: DiGraph::new() }
    }

    /// Stages a bare node with payload `kind` and no inputs/outputs; returns
    /// its node id.
    pub(crate) fn add_node(&mut self, kind: N) -> usize {
        self.g
            .add_node(StagedNode {
                kind,
                outputs: Vec::new(),
                inputs: Vec::new(),
            })
            .index()
    }

    /// Appends output payload `out` to `node`; returns its output slot index.
    pub(crate) fn add_output(&mut self, node: usize, out: V) -> usize {
        let w = &mut self.g[NodeIndex::new(node)];
        let slot = w.outputs.len();
        w.outputs.push(out);
        slot
    }

    /// Wires producer `(prod_node, prod_output)` into `consumer`'s input
    /// `slot`. Records both the dependency edge (for the toposort) and the
    /// ordered `(slot, producer)` triple (for materialisation).
    pub(crate) fn add_input(
        &mut self,
        consumer: usize,
        slot: usize,
        prod_node: usize,
        prod_output: usize,
    ) {
        self.g[NodeIndex::new(consumer)]
            .inputs
            .push((slot, prod_node, prod_output));
        self.g
            .add_edge(NodeIndex::new(prod_node), NodeIndex::new(consumer), ());
    }

    /// Mutable access to `node`'s payload (for the side-specific annotators).
    pub(crate) fn kind_mut(&mut self, node: usize) -> &mut N {
        &mut self.g[NodeIndex::new(node)].kind
    }

    /// Mutable access to `node`'s output payload at `output`.
    pub(crate) fn output_mut(&mut self, node: usize, output: usize) -> &mut V {
        &mut self.g[NodeIndex::new(node)].outputs[output]
    }

    /// The `(producer_node, producer_output)` of each input of `node`, in
    /// wire order.
    pub(crate) fn input_producers(&self, node: usize) -> Vec<(usize, usize)> {
        self.g[NodeIndex::new(node)]
            .inputs
            .iter()
            .map(|&(_slot, pn, po)| (pn, po))
            .collect()
    }

    /// Materialises the staged DAG into a sealed [`strider_graph::Graph`] in
    /// producer-before-consumer order (`petgraph::algo::toposort`). Each
    /// staged payload is bridged to the sealed payload via [`SealNode`],
    /// stamped with its recovered consumer-slot list.
    ///
    /// # Errors
    /// Errors on a cyclic staged graph (a builder bug — pattern / template
    /// graphs are always DAGs).
    #[allow(clippy::expect_used)]
    pub(crate) fn seal(self) -> anyhow::Result<Graph<N::Sealed, V, NeverCacheable>>
    where
        N: SealNode,
    {
        let order = toposort(&self.g, None).map_err(|c| {
            anyhow!(
                "cycle in staged pattern/template graph at {:?}",
                c.node_id()
            )
        })?;
        // Own each weight, indexable by node id; `take`n exactly once, in
        // topo order.
        let mut weights: Vec<Option<StagedNode<N, V>>> = self
            .g
            .into_nodes_edges()
            .0
            .into_iter()
            .map(|n| Some(n.weight))
            .collect();
        let mut graph: Graph<N::Sealed, V, NeverCacheable> = Graph::new();
        let mut materialised: Vec<Vec<ValueId>> = vec![Vec::new(); weights.len()];
        for ix in order {
            let i = ix.index();
            let StagedNode {
                kind,
                outputs,
                inputs,
            } = weights[i].take().expect("each node materialised once");
            let mut input_values: Vec<ValueId> = Vec::with_capacity(inputs.len());
            let mut input_slots: Vec<usize> = Vec::with_capacity(inputs.len());
            for (slot, prod_node, prod_output) in inputs {
                input_values.push(materialised[prod_node][prod_output]);
                input_slots.push(slot);
            }
            let node_id = graph.create_node(kind.seal(input_slots), input_values, outputs);
            materialised[i] = graph.node_outputs(node_id).to_vec();
        }
        Ok(graph)
    }
}
