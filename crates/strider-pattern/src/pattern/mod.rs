//! The bipartite pattern graph.
//!
//! [`Pattern`] mirrors the IR's `Node → NodeOutput → Node` structure
//! with two vertex kinds: [`PatNode`] (an IR node) and [`PatOutput`]
//! (a node output). Edges are [`PatEdge`]: `Produces` (node → its
//! output) and `Consumes` (output → consuming node at a slot).

// `dead_code` allow: several accessors / constructors here are consumed
// only by the builder + matcher engines landing in later changes; this
// crate's lints run with `-D warnings`.
#![allow(dead_code)]

mod edge;
mod topo;
mod vertex;

pub use edge::PatEdge;
pub use vertex::{KindSpec, LocalLimit, OutputKindSpec, PatNode, PatOutput, PostMatchFn};

// Re-exported for the builder + template engines in later changes; the
// inline topo test is the only current consumer.
#[allow(unused_imports)]
pub(crate) use topo::{assert_dag, reachable_topo};

use petgraph::stable_graph::{NodeIndex, StableDiGraph};

use crate::matcher::CastMask;

/// A vertex in the bipartite pattern graph: either a node or one of a
/// node's outputs.
pub enum PatVertex {
    /// An IR-node-shaped vertex.
    Node(PatNode),
    /// A node-output-shaped vertex.
    Output(PatOutput),
}

/// A pattern over the IR, stored as a bipartite directed graph.
pub struct Pattern {
    pub(crate) inner: StableDiGraph<PatVertex, PatEdge>,
    pub(crate) root: Option<NodeIndex>,
    pub(crate) cast_mask: CastMask,
}

impl Default for Pattern {
    fn default() -> Self {
        Self::new()
    }
}

impl Pattern {
    /// An empty pattern with no root and no cast walk-through.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: StableDiGraph::new(),
            root: None,
            cast_mask: CastMask::empty(),
        }
    }

    /// Adds a node vertex, returning its index.
    pub fn add_node(&mut self, n: PatNode) -> NodeIndex {
        self.inner.add_node(PatVertex::Node(n))
    }

    /// Adds an output vertex produced by `producer`, returning its
    /// index.
    pub fn add_output(&mut self, producer: NodeIndex, o: PatOutput) -> NodeIndex {
        let idx = self.inner.add_node(PatVertex::Output(o));
        self.inner.add_edge(producer, idx, PatEdge::Produces);
        idx
    }

    /// Wires `output` into `consumer`'s input `slot`.
    pub fn consume(&mut self, consumer: NodeIndex, slot: usize, output: NodeIndex) {
        self.inner.add_edge(output, consumer, PatEdge::Consumes { slot });
    }

    /// Sets the pattern's root node.
    pub fn set_root(&mut self, n: NodeIndex) {
        self.root = Some(n);
    }

    /// The pattern's root node, if set.
    #[must_use]
    pub fn root(&self) -> Option<NodeIndex> {
        self.root
    }

    /// Adds `m` to the cast-walk-through mask (the matcher transparently
    /// skips the selected cast kinds).
    #[must_use]
    pub fn ignore_casts_mask(mut self, m: CastMask) -> Self {
        self.cast_mask |= m;
        self
    }

    /// Walks through every value-passthrough cast kind during matching.
    #[must_use]
    pub fn ignore_casts(self) -> Self {
        self.ignore_casts_mask(CastMask::all())
    }

    /// Number of node vertices.
    #[must_use]
    pub fn node_count(&self) -> usize {
        self.inner
            .node_weights()
            .filter(|v| matches!(v, PatVertex::Node(_)))
            .count()
    }

    /// Number of output vertices.
    #[must_use]
    pub fn output_count(&self) -> usize {
        self.inner
            .node_weights()
            .filter(|v| matches!(v, PatVertex::Output(_)))
            .count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use strider_ir::node::NodeKind;

    #[test]
    fn builds_bipartite_add_shape() {
        let mut p = Pattern::new();
        let kx = p.add_node(PatNode::wildcard());
        let xout = p.add_output(kx, PatOutput::value(0));
        let kk = p.add_node(PatNode::exact(NodeKind::IntConst(1)));
        let kout = p.add_output(kk, PatOutput::value(0));
        let add = p.add_node(PatNode::exact(NodeKind::IntBinaryOp(
            strider_ir::IntBinaryOp::Add,
        )));
        p.consume(add, 0, xout);
        p.consume(add, 1, kout);
        let _addout = p.add_output(add, PatOutput::value(0));
        p.set_root(add);
        assert_eq!(p.node_count(), 3);
        assert_eq!(p.output_count(), 3);
        assert!(p.root().is_some());
    }
}
