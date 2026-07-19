//! petgraph trait impls over the bipartite [`Vertex`] view.
//!
//! One petgraph "node" is a [`Vertex`], and the directed edge relation is:
//!
//! - `Node(n)`  -> `Value(v)` for each output `v` of `n`.
//! - `Value(v)` -> `Node(c)` for each consumer `c` of `v` (via the use-list).
//!
//! Under that relation a producer node always precedes its consumers in a
//! topological order, so `toposort` and `DfsPostOrder` (plus `Reversed`) work
//! directly on the graph.
//!
//! The impls live on `&Graph` because petgraph requires `GraphRef: Copy`.

use petgraph::Direction;
use petgraph::visit::{
    GraphBase, IntoNeighbors, IntoNeighborsDirected, IntoNodeIdentifiers, Visitable,
};
use rustc_hash::FxHashSet;

use crate::cache::NodeCacheable;
use crate::graph::Graph;
use crate::ids::Vertex;

impl<N, V, C: NodeCacheable<N, V>> GraphBase for Graph<N, V, C> {
    type NodeId = Vertex;
    type EdgeId = (Vertex, Vertex);
}

impl<N, V, C: NodeCacheable<N, V>> Graph<N, V, C> {
    fn out_neighbors(&self, v: Vertex) -> std::vec::IntoIter<Vertex> {
        let out: Vec<Vertex> = match v {
            Vertex::Node(n) => self
                .node_outputs(n)
                .iter()
                .map(|&out| Vertex::Value(out))
                .collect(),
            Vertex::Value(val) => {
                let mut seen: FxHashSet<crate::ids::NodeId> = FxHashSet::default();
                self.value_uses(val)
                    .filter_map(|(consumer, _)| {
                        seen.insert(consumer).then_some(Vertex::Node(consumer))
                    })
                    .collect()
            }
        };
        out.into_iter()
    }

    /// The reverse of [`Self::out_neighbors`].
    fn in_neighbors(&self, v: Vertex) -> std::vec::IntoIter<Vertex> {
        let inc: Vec<Vertex> = match v {
            Vertex::Value(val) => vec![Vertex::Node(self.producer(val))],
            Vertex::Node(n) => {
                let mut seen: FxHashSet<crate::ids::ValueId> = FxHashSet::default();
                self.node_inputs(n)
                    .into_iter()
                    .filter_map(|val| seen.insert(val).then_some(Vertex::Value(val)))
                    .collect()
            }
        };
        inc.into_iter()
    }
}

impl<N, V, C: NodeCacheable<N, V>> IntoNeighbors for &Graph<N, V, C> {
    type Neighbors = std::vec::IntoIter<Vertex>;

    fn neighbors(self, a: Vertex) -> Self::Neighbors {
        self.out_neighbors(a)
    }
}

impl<N, V, C: NodeCacheable<N, V>> IntoNeighborsDirected for &Graph<N, V, C> {
    type NeighborsDirected = std::vec::IntoIter<Vertex>;

    fn neighbors_directed(self, n: Vertex, d: Direction) -> Self::NeighborsDirected {
        match d {
            Direction::Outgoing => self.out_neighbors(n),
            Direction::Incoming => self.in_neighbors(n),
        }
    }
}

impl<N, V, C: NodeCacheable<N, V>> IntoNodeIdentifiers for &Graph<N, V, C> {
    type NodeIdentifiers = std::vec::IntoIter<Vertex>;

    fn node_identifiers(self) -> Self::NodeIdentifiers {
        let mut ids: Vec<Vertex> = Vec::new();
        for node in self.all_node_ids() {
            ids.push(Vertex::Node(node));
            for &out in self.node_outputs(node) {
                ids.push(Vertex::Value(out));
            }
        }
        ids.into_iter()
    }
}

impl<N, V, C: NodeCacheable<N, V>> Visitable for &Graph<N, V, C> {
    type Map = FxHashSet<Vertex>;

    fn visit_map(&self) -> Self::Map {
        FxHashSet::default()
    }

    fn reset_map(&self, map: &mut Self::Map) {
        map.clear();
    }
}
