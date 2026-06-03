//! The bipartite match pattern.
//!
//! [`Pattern`] is a thin instantiation of the generic
//! [`BiGraph`] over the match payloads
//! [`PatNode`] (an IR node) and [`PatValue`] (a node output), plus a
//! cast-walk-through mask. The shared graph mechanics (vertices, edges,
//! reachable-topo) live in [`crate::bigraph`]; this module owns only the
//! match-side payloads and the cast mask.

mod vertex;

pub use vertex::{
    KindSpec, NodePredicate, OutputKindSpec, PatNode, PatValue, PostMatchFn, ValuePredicate,
};

use petgraph::stable_graph::NodeIndex;

use crate::bigraph::BiGraph;
use crate::matcher::CastMask;

/// A pattern over the IR: a bipartite [`BiGraph`] of [`PatNode`] /
/// [`PatValue`] vertices plus a cast-walk-through mask.
pub struct Pattern {
    pub(crate) graph: BiGraph<PatNode, PatValue>,
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
            graph: BiGraph::new(),
            cast_mask: CastMask::empty(),
        }
    }

    /// Adds a node vertex, returning its index.
    pub fn add_node(&mut self, n: PatNode) -> NodeIndex {
        self.graph.add_node(n)
    }

    /// Adds an output vertex produced by `producer`, returning its
    /// index.
    pub fn add_output(&mut self, producer: NodeIndex, o: PatValue) -> NodeIndex {
        self.graph.add_output(producer, o)
    }

    /// Wires `output` into `consumer`'s input `slot`.
    pub fn consume(&mut self, consumer: NodeIndex, slot: usize, output: NodeIndex) {
        self.graph.consume(consumer, slot, output);
    }

    /// The pattern's match root — the unique graph sink, recovered
    /// structurally, after confirming the reachable graph is acyclic.
    ///
    /// # Errors
    /// Errors if the pattern is not a single-rooted, acyclic graph the
    /// matcher can handle: zero sinks (rootless / cyclic), more than one
    /// sink (multi-rooted — a valid graph a user can build via shared
    /// captures, but not yet matchable), or a cycle in the root's input
    /// cone.
    pub fn root(&self) -> anyhow::Result<NodeIndex> {
        let root = self.graph.derive_root()?;
        crate::bigraph::assert_dag(&self.graph, root)?;
        Ok(root)
    }

    /// Attaches a post-match closure to the pattern's root node.
    ///
    /// The matcher runs the root [`PatNode`]'s `post_match` hook after the
    /// whole pattern (root + all inputs) has matched, returning `false`
    /// to reject the match. This is the finished-`Pattern` analogue of the
    /// builder-side `when_match` combinator: a control / variadic builder
    /// finalises straight to a `Pattern` (with no value-output `MatchPat`
    /// form to wrap), so this is the only way to attach a root guard to it.
    ///
    /// # Panics
    ///
    /// Panics if the pattern has no unique sink root, or if the root index
    /// does not resolve to a node vertex (both are construction invariants
    /// a finished pattern always upholds).
    #[allow(clippy::expect_used)]
    pub fn set_root_post_match(&mut self, f: PostMatchFn) {
        let root = self
            .graph
            .derive_root()
            .expect("pattern has a unique sink root");
        let nd = self
            .graph
            .node_weight_mut(root)
            .expect("root index must resolve to a node vertex");
        nd.post_match = Some(f);
    }

    /// Builder form of [`Pattern::set_root_post_match`]: attaches a root
    /// post-match closure and returns the pattern.
    #[must_use]
    pub fn with_root_post_match(mut self, f: PostMatchFn) -> Self {
        self.set_root_post_match(f);
        self
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
        self.graph.node_count()
    }

    /// Number of output vertices.
    #[must_use]
    pub fn output_count(&self) -> usize {
        self.graph.output_count()
    }

    /// Number of control-output vertices. Used to assert the `If`
    /// representation invariant (two control outputs: true + false).
    #[must_use]
    pub fn control_output_count(&self) -> usize {
        self.graph
            .output_weights()
            .filter(|o| matches!(o.kind, OutputKindSpec::Control))
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
        let xout = p.add_output(kx, PatValue::value(0));
        let kk = p.add_node(PatNode::exact(NodeKind::IntConst(1)));
        let kout = p.add_output(kk, PatValue::value(0));
        let add = p.add_node(PatNode::exact(NodeKind::IntBinaryOp(
            strider_ir::IntBinaryOp::Add,
        )));
        p.consume(add, 0, xout);
        p.consume(add, 1, kout);
        let _addout = p.add_output(add, PatValue::value(0));
        assert_eq!(p.node_count(), 3);
        assert_eq!(p.output_count(), 3);
        // The root is derived as the unique sink (`add`).
        assert_eq!(p.root().unwrap(), add);
    }

    #[test]
    fn reachable_topo_orders_producers_before_consumers() {
        use crate::bigraph::reachable_topo;
        let mut p = Pattern::new();
        let a = p.add_node(PatNode::wildcard());
        let ao = p.add_output(a, PatValue::value(0));
        let b = p.add_node(PatNode::wildcard());
        let _bout = p.add_output(b, PatValue::value(0));
        p.consume(b, 0, ao);
        let order = reachable_topo(&p.graph, p.root().unwrap()).unwrap();
        let pa = order.iter().position(|&n| n == a).unwrap();
        let pb = order.iter().position(|&n| n == b).unwrap();
        assert!(pa < pb);
    }
}
