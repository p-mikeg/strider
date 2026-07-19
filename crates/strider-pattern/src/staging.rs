//! A DAG is built incrementally: a bare node is staged, outputs are added
//! later, inputs are wired once the producers exist. Sealing materialises the
//! whole DAG in `toposort` order.
//!
//! Input order and each input's sparse consumer slot ride on the node weight,
//! because petgraph edge iteration order is unspecified.

use anyhow::anyhow;
use petgraph::algo::toposort;
use petgraph::graph::{DiGraph, NodeIndex};
use strider_graph::{Graph, NeverCacheable, ValueId};

/// `outputs` is in slot-add order; `inputs` is
/// `(consumer_slot, producer_node, producer_output)` in wire order.
struct StagedNode<N, V> {
    kind: N,
    outputs: Vec<V>,
    inputs: Vec<(usize, usize, usize)>,
}

/// Bridges a staged node payload to its sealed-graph payload, stamping on the
/// recovered sparse consumer-slot list.
///
/// The match side seals `PatNode` into itself, filling its `input_slots`
/// field; the build side seals `TmplNodeKind` into
/// `TmplNode { kind, input_slots }`.
pub(crate) trait SealNode {
    type Sealed;
    fn seal(self, input_slots: Vec<usize>) -> Self::Sealed;
}

/// Node identity is a plain `usize`, interchangeable with a petgraph
/// [`NodeIndex`] because staging never removes nodes.
pub(crate) struct StagedGraph<N, V> {
    g: DiGraph<StagedNode<N, V>, ()>,
}

impl<N, V> Default for StagedGraph<N, V> {
    fn default() -> Self {
        Self::new()
    }
}

impl<N, V> StagedGraph<N, V> {
    pub(crate) fn new() -> Self {
        Self { g: DiGraph::new() }
    }

    pub(crate) fn add_node(&mut self, kind: N) -> usize {
        self.g
            .add_node(StagedNode {
                kind,
                outputs: Vec::new(),
                inputs: Vec::new(),
            })
            .index()
    }

    /// Returns the new output's slot index.
    pub(crate) fn add_output(&mut self, node: usize, out: V) -> usize {
        let w = &mut self.g[NodeIndex::new(node)];
        let slot = w.outputs.len();
        w.outputs.push(out);
        slot
    }

    /// Records the dependency edge and the ordered `(slot, producer)` triple.
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

    /// For the side-specific annotators.
    pub(crate) fn kind_mut(&mut self, node: usize) -> &mut N {
        &mut self.g[NodeIndex::new(node)].kind
    }

    pub(crate) fn output_mut(&mut self, node: usize, output: usize) -> &mut V {
        &mut self.g[NodeIndex::new(node)].outputs[output]
    }

    /// In wire order.
    pub(crate) fn input_producers(&self, node: usize) -> Vec<(usize, usize)> {
        self.g[NodeIndex::new(node)]
            .inputs
            .iter()
            .map(|&(_slot, pn, po)| (pn, po))
            .collect()
    }

    /// Materialises in producer-before-consumer order, bridging each staged
    /// payload via [`SealNode`].
    ///
    /// # Errors
    /// On a cyclic staged graph, which is a builder bug: pattern and template
    /// graphs are always DAGs.
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
        // Owned, indexable by node id, `take`n exactly once in topo order.
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
