//! A DAG is built incrementally: a bare node is staged, outputs are added
//! later, inputs are wired once the producers exist. Sealing materialises the
//! whole DAG in `toposort` order.
//!
//! Input order and each input's sparse consumer slot ride on the node weight,
//! because petgraph edge iteration order is unspecified.

use petgraph::algo::toposort;
use petgraph::graph::{DiGraph, NodeIndex};
use strider_graph::{Graph, NeverCacheable, ValueId};

/// `outputs` is in slot-add order; `inputs` is
/// `(consumer_slot, producer_node, producer_output)` in wire order.
struct StagedNode<N: SealNode, V> {
    kind: N,
    outputs: Vec<V>,
    inputs: Vec<(N::Slot, usize, usize)>,
}

/// Bridges a staged node payload to its sealed-graph payload, stamping on the
/// recovered sparse consumer-slot list.
///
/// The match side seals `PatNode` into itself, filling its `input_slots`
/// field; the build side seals `TmplNodeKind` into
/// `TmplNode { kind, input_slots }`.
pub(crate) trait SealNode {
    type Sealed;
    /// A staged input's consumer slot, recorded on the sealed payload.
    type Slot: Copy;
    fn seal(self, input_slots: Vec<Self::Slot>) -> Self::Sealed;
}

/// Node identity is a plain `usize`, interchangeable with a petgraph
/// [`NodeIndex`] because staging never removes nodes.
pub(crate) struct StagedGraph<N: SealNode, V> {
    g: DiGraph<StagedNode<N, V>, ()>,
    /// The first build-time refusal. A staged graph is untrusted input, so
    /// refusing one is a `Result`, never a panic.
    refusal: Option<String>,
}

impl<N: SealNode, V> Default for StagedGraph<N, V> {
    fn default() -> Self {
        Self::new()
    }
}

impl<N: SealNode, V> StagedGraph<N, V> {
    pub(crate) fn new() -> Self {
        Self {
            g: DiGraph::new(),
            refusal: None,
        }
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
        slot: N::Slot,
        prod_node: usize,
        prod_output: usize,
    ) {
        self.g[NodeIndex::new(consumer)]
            .inputs
            .push((slot, prod_node, prod_output));
        self.g
            .add_edge(NodeIndex::new(prod_node), NodeIndex::new(consumer), ());
    }

    /// Keeps the first refusal; later ones are dropped.
    pub(crate) fn reject(&mut self, why: String) {
        self.refusal.get_or_insert(why);
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
    /// payload via [`SealNode`], paired with the refusal to report: the first
    /// one recorded, else a cycle, which seals to an empty graph.
    pub(crate) fn seal(self) -> (Graph<N::Sealed, V, NeverCacheable>, Option<String>) {
        let order = match toposort(&self.g, None) {
            Ok(order) => order,
            Err(c) => {
                let cycle = format!(
                    "cycle in staged pattern/template graph at {:?}",
                    c.node_id()
                );
                return (Graph::new(), Some(self.refusal.unwrap_or(cycle)));
            }
        };
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
            let mut input_slots: Vec<N::Slot> = Vec::with_capacity(inputs.len());
            for (slot, prod_node, prod_output) in inputs {
                input_values.push(materialised[prod_node][prod_output]);
                input_slots.push(slot);
            }
            let node_id = graph.create_node(kind.seal(input_slots), input_values, outputs);
            materialised[i] = graph.node_outputs(node_id).to_vec();
        }
        (graph, self.refusal)
    }
}
