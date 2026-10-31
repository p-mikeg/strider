//! A DAG is built incrementally: a bare node is staged, outputs are added
//! later, inputs are wired once the producers exist. Sealing materialises the
//! whole DAG in petgraph's `toposort` order.

use petgraph::algo::toposort;
use petgraph::graph::{DiGraph, NodeIndex};
use strider_graph::{Graph, NeverCacheable, ValueId};

/// `first` then `more` are the outputs in slot-add order, the first held
/// inline because nearly every node has exactly one; `inputs` is
/// `(consumer_slot, producer_node, producer_output)` in wire order.
struct StagedNode<N: SealNode, V> {
    kind: N,
    first: Option<V>,
    more: Vec<V>,
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

/// Node identity is a plain `usize`, the staging position, interchangeable
/// with a petgraph [`NodeIndex`] because staging never removes nodes.
pub(crate) struct StagedGraph<N: SealNode, V> {
    nodes: Vec<StagedNode<N, V>>,
    /// `(producer, consumer)` per wiring, in wire order.
    edges: Vec<(usize, usize)>,
    /// The first build-time refusal. A staged graph is untrusted input, so
    /// refusing one is a `Result`, never a panic.
    refusal: Option<String>,
    /// Nesting levels being lowered; see [`Self::enter_nesting`].
    levels: usize,
    /// Recursion steps being lowered, levels or not.
    depth: usize,
    /// Frame address of the outermost live step, the zero the stack
    /// consumption is measured against.
    stack_base: usize,
}

/// Stack one lowering may consume before [`StagedGraph::enter_nesting`]
/// bails. A level count alone does not bound stack: a level costs one frame per
/// builder and decorator it passes through, and an unoptimised frame is several
/// times an optimised one.
///
/// The fattest shape measured, an unoptimised alternation of a decorated call,
/// reaches [`MAX_PATTERN_NODES`](crate::matcher::graph::MAX_PATTERN_NODES)
/// levels inside 0.75 MiB, so the level cap stays the bound a normal pattern
/// trips on; this catches a fatter one before the 2 MiB a spawned thread gets
/// by default runs out.
const MAX_PATTERN_STACK: usize = 1024 * 1024;

/// Address of a local in the calling frame. The stack grows DOWN on every
/// target this ships to, so a deeper frame reads lower; `saturating_sub` is
/// what makes the other direction read as no consumption rather than as a huge
/// one.
#[inline(never)]
fn frame_mark() -> usize {
    let probe = 0u8;
    std::hint::black_box(std::ptr::from_ref(&probe)) as usize
}

impl<N: SealNode, V> Default for StagedGraph<N, V> {
    fn default() -> Self {
        Self::new()
    }
}

impl<N: SealNode, V> StagedGraph<N, V> {
    pub(crate) fn new() -> Self {
        Self {
            nodes: Vec::new(),
            edges: Vec::new(),
            refusal: None,
            levels: 0,
            depth: 0,
            stack_base: 0,
        }
    }

    /// Opens one recursion step of lowering, refusing once the stack budget is
    /// spent and, for a `level`, past `MAX_PATTERN_NODES` levels. Lowering
    /// recurses before any node exists to count, so the cap at seal comes too
    /// late to keep the recursion off the stack; a level past it is over the
    /// node cap anyway. A step that adds no node (a decoration) is charged to
    /// the stack alone.
    pub(crate) fn enter_nesting(&mut self, level: bool) -> bool {
        let here = frame_mark();
        let max = crate::matcher::graph::MAX_PATTERN_NODES;
        if level && self.levels >= max {
            self.reject(format!(
                "pattern nests more than {max} levels deep, over what lowering can \
                 recurse through"
            ));
            return false;
        }
        if self.depth == 0 {
            self.stack_base = here;
        } else {
            let used = self.stack_base.saturating_sub(here);
            if used > MAX_PATTERN_STACK {
                self.reject(format!(
                    "a pattern or template nests too deep for this thread's stack ({} KiB \
                     used of a {} KiB lowering budget, {} steps down); nest less, or lower \
                     it on a thread with a larger stack",
                    used / 1024,
                    MAX_PATTERN_STACK / 1024,
                    self.depth,
                ));
                return false;
            }
        }
        self.depth += 1;
        self.levels += usize::from(level);
        true
    }

    pub(crate) fn leave_nesting(&mut self, level: bool) {
        self.depth -= 1;
        self.levels -= usize::from(level);
    }

    pub(crate) fn add_node(&mut self, kind: N) -> usize {
        self.nodes.push(StagedNode {
            kind,
            first: None,
            more: Vec::new(),
            inputs: Vec::new(),
        });
        self.nodes.len() - 1
    }

    /// Returns the new output's slot index.
    pub(crate) fn add_output(&mut self, node: usize, out: V) -> usize {
        let w = &mut self.nodes[node];
        if w.first.is_none() {
            w.first = Some(out);
            return 0;
        }
        w.more.push(out);
        w.more.len()
    }

    /// Records the dependency edge and the ordered `(slot, producer)` triple.
    pub(crate) fn add_input(
        &mut self,
        consumer: usize,
        slot: N::Slot,
        prod_node: usize,
        prod_output: usize,
    ) {
        self.nodes[consumer]
            .inputs
            .push((slot, prod_node, prod_output));
        self.edges.push((prod_node, consumer));
    }

    /// Keeps the first refusal; later ones are dropped.
    pub(crate) fn reject(&mut self, why: String) {
        self.refusal.get_or_insert(why);
    }

    /// For the side-specific annotators.
    pub(crate) fn kind_mut(&mut self, node: usize) -> &mut N {
        &mut self.nodes[node].kind
    }

    pub(crate) fn output_mut(&mut self, node: usize, output: usize) -> &mut V {
        let w = &mut self.nodes[node];
        match output {
            0 => w.first.as_mut().expect("output 0 is staged"),
            n => &mut w.more[n - 1],
        }
    }

    /// Materialises in producer-before-consumer order, bridging each staged
    /// payload via [`SealNode`], paired with the refusal to report: the first
    /// one recorded, else a cycle, which seals to an empty graph.
    pub(crate) fn seal(self) -> (Graph<N::Sealed, V, NeverCacheable>, Option<String>) {
        let order = match self.order() {
            Ok(order) => order,
            Err(c) => {
                let cycle = format!("cycle in staged pattern/template graph at {c:?}");
                return (Graph::new(), Some(self.refusal.unwrap_or(cycle)));
            }
        };
        // Owned, indexable by node id, `take`n exactly once in topo order.
        let mut weights: Vec<Option<StagedNode<N, V>>> = self.nodes.into_iter().map(Some).collect();
        let mut graph: Graph<N::Sealed, V, NeverCacheable> = Graph::new();
        let mut sealed: Vec<Option<strider_graph::NodeId>> = vec![None; weights.len()];
        let mut input_values: Vec<ValueId> = Vec::new();
        for i in order {
            let StagedNode {
                kind,
                first,
                more,
                inputs,
            } = weights[i].take().expect("each node materialised once");
            let input_slots: Vec<N::Slot> = inputs.iter().map(|&(slot, _, _)| slot).collect();
            input_values.extend(inputs.iter().map(|&(_, node, output)| {
                let producer = sealed[node].expect("a producer seals before its consumers");
                graph.node_outputs(producer)[output]
            }));
            let outputs = first.into_iter().chain(more);
            let node = graph.create_node(kind.seal(input_slots), input_values.drain(..), outputs);
            sealed[i] = Some(node);
        }
        (graph, self.refusal)
    }

    /// Producer-before-consumer positions, as petgraph's sort orders the
    /// staged graph.
    fn order(&self) -> Result<Vec<usize>, NodeIndex> {
        if let Some(order) = self.tree_order() {
            return Ok(order);
        }
        let mut g: DiGraph<(), ()> = DiGraph::with_capacity(self.nodes.len(), self.edges.len());
        for _ in &self.nodes {
            g.add_node(());
        }
        for &(p, c) in &self.edges {
            g.add_edge(NodeIndex::new(p), NodeIndex::new(c), ());
        }
        toposort(&g, None)
            .map(|o| o.into_iter().map(NodeIndex::index).collect())
            .map_err(|c| c.node_id())
    }

    /// petgraph's sort without the graph, for the shape nearly every pattern
    /// has: each node consumed at most once, every edge running the same way
    /// between staging positions, so no cycle. The sort walks the nodes in
    /// position order, each up its consumer chain to a node already seen,
    /// finishes the chain top first, and reverses the finish order.
    fn tree_order(&self) -> Option<Vec<usize>> {
        let n = self.nodes.len();
        let backward = self.edges.first().is_some_and(|&(p, c)| p > c);
        let mut consumer = vec![usize::MAX; n];
        for &(p, c) in &self.edges {
            if (p > c) != backward || p == c || consumer[p] != usize::MAX {
                return None;
            }
            consumer[p] = c;
        }
        let mut seen = vec![false; n];
        let mut finished = Vec::with_capacity(n);
        for start in 0..n {
            let at = finished.len();
            let mut x = start;
            while x != usize::MAX && !seen[x] {
                seen[x] = true;
                finished.push(x);
                x = consumer[x];
            }
            finished[at..].reverse();
        }
        finished.reverse();
        Some(finished)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Unit;
    impl SealNode for Unit {
        type Sealed = Unit;
        type Slot = usize;
        fn seal(self, _: Vec<usize>) -> Unit {
            Unit
        }
    }

    /// The graph-free sort of a tree is petgraph's, whichever way the edges
    /// run between staging positions.
    #[test]
    fn the_tree_order_is_petgraphs() {
        let mut seed = 0x9E37_79B9_7F4A_7C15u64;
        let mut next = |n: usize| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed % n as u64) as usize
        };
        let mut checked = 0;
        for _ in 0..2000 {
            let n = 1 + next(12);
            let backward = next(2) == 0;
            let mut s: StagedGraph<Unit, ()> = StagedGraph::new();
            for _ in 0..n {
                s.add_node(Unit);
            }
            // Each node but one root takes a consumer on its side of it.
            for p in 0..n {
                let c = if backward {
                    (p > 0).then(|| next(p))
                } else {
                    (p + 1 < n).then(|| p + 1 + next(n - p - 1))
                };
                if let Some(c) = c.filter(|_| next(4) != 0) {
                    s.add_input(c, 0, p, 0);
                }
            }
            let Some(fast) = s.tree_order() else { continue };
            let mut g: DiGraph<(), ()> = DiGraph::new();
            for _ in 0..n {
                g.add_node(());
            }
            for &(p, c) in &s.edges {
                g.add_edge(NodeIndex::new(p), NodeIndex::new(c), ());
            }
            let sorted: Vec<usize> = toposort(&g, None)
                .unwrap()
                .into_iter()
                .map(NodeIndex::index)
                .collect();
            assert_eq!(fast, sorted, "edges {:?}", s.edges);
            checked += 1;
        }
        assert!(checked > 1000);
    }
}
