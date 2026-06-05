//! The bipartite match pattern.
//!
//! [`Pattern`] wraps the generic [`strider_graph::Graph`] (with the
//! always-allocate [`NeverCacheable`](strider_graph::NeverCacheable) policy)
//! over the match payloads [`PatNode`] (an IR node) and [`PatValue`] (a node
//! output), plus a cast-walk-through mask. The BiGraph-era read vocabulary
//! the matcher uses (`consumed_inputs` with per-edge slots, `derive_root`,
//! …) is restored on the generic graph by [`crate::graph_ext`]; the
//! imperative construction lives in
//! [`MatcherBuilder`](crate::matcher::MatcherBuilder), which stages nodes and
//! materialises them into the generic graph at seal time.

use strider_graph::{Graph, NeverCacheable, NodeId};

use super::CastMask;
use super::vertex::{PatNode, PatValue, PostMatchFn};
use crate::graph_ext::PatGraphRead;

/// The generic graph backing a [`Pattern`] / the [`MatcherBuilder`].
pub(crate) type PatGraph = Graph<PatNode, PatValue, NeverCacheable>;

/// A pattern over the IR: a generic bipartite graph of [`PatNode`] /
/// [`PatValue`] vertices plus a cast-walk-through mask.
pub struct Pattern {
    pub(crate) graph: PatGraph,
    pub(crate) cast_mask: CastMask,
}

impl Default for Pattern {
    fn default() -> Self {
        Self::new()
    }
}

impl Pattern {
    /// An empty pattern with no root and no cast walk-through.
    pub fn new() -> Self {
        Self {
            graph: Graph::new(),
            cast_mask: CastMask::empty(),
        }
    }

    /// Build a pattern from an already-materialised generic graph (the seal
    /// point of [`MatcherBuilder`]).
    pub(crate) fn from_graph(graph: PatGraph) -> Self {
        Self {
            graph,
            cast_mask: CastMask::empty(),
        }
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
    pub fn root(&self) -> anyhow::Result<NodeId> {
        let root = self.graph.derive_root()?;
        // Confirm the reachable input cone is acyclic (errors on a cycle).
        crate::graph_ext::reachable_topo(&self.graph, root)?;
        Ok(root)
    }

    /// Every [`Capture`](crate::capture::Capture) this pattern binds.
    ///
    /// Captures live on the value side (the producing output vertex) for
    /// value captures, and on the node for value-less roots — both are
    /// collected here. Used by the rewrite engine's construction-time
    /// capture-coverage check.
    pub fn bound_captures(&self) -> impl Iterator<Item = crate::capture::Capture> + '_ {
        self.graph
            .all_node_ids()
            .filter_map(|n| self.graph.node_kind(n).capture)
            .chain(
                self.graph
                    .all_value_ids()
                    .filter_map(|v| self.graph.value_kind_ref(v).capture),
            )
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
    /// Panics if the pattern has no unique sink root (a construction
    /// invariant a finished pattern always upholds).
    #[allow(clippy::expect_used)]
    pub(crate) fn set_root_post_match(&mut self, f: PostMatchFn) {
        let root = self
            .graph
            .derive_root()
            .expect("pattern has a unique sink root");
        self.graph.node_kind_mut(root).post_match = Some(f);
    }

    /// Builder form of `Pattern::set_root_post_match`: attaches a root
    /// post-match closure and returns the pattern.
    pub fn with_root_post_match(mut self, f: PostMatchFn) -> Self {
        self.set_root_post_match(f);
        self
    }

    /// Adds `m` to the cast-walk-through mask (the matcher transparently
    /// skips the selected cast kinds).
    pub fn ignore_casts_mask(mut self, m: CastMask) -> Self {
        self.cast_mask |= m;
        self
    }

    /// Walks through every value-passthrough cast kind during matching.
    pub fn ignore_casts(self) -> Self {
        self.ignore_casts_mask(CastMask::all())
    }

    /// Number of node vertices. Test-only structural accessor.
    #[cfg(test)]
    pub(crate) fn node_count(&self) -> usize {
        self.graph.all_node_ids().count()
    }

    /// Number of output vertices. Test-only structural accessor.
    #[cfg(test)]
    pub(crate) fn output_count(&self) -> usize {
        self.graph.all_value_ids().count()
    }

    /// Number of control-output vertices. Used to assert the `If`
    /// representation invariant (two control outputs: true + false).
    /// Test-only structural accessor.
    #[cfg(test)]
    pub(crate) fn control_output_count(&self) -> usize {
        self.graph
            .all_value_ids()
            .filter(|&v| {
                matches!(
                    self.graph.value_kind_ref(v).kind,
                    super::vertex::OutputKindSpec::Control
                )
            })
            .count()
    }
}

#[cfg(test)]
mod tests {
    use crate::matcher::MatcherBuilder;
    use strider_ir::node::NodeKind;

    #[test]
    fn builds_bipartite_add_shape() {
        let mut b = MatcherBuilder::new();
        let x = b.leaf(crate::matcher::KindSpec::Any);
        let k = b.leaf(crate::matcher::KindSpec::Exact(NodeKind::IntConst(1)));
        let _sum = b.binary(strider_ir::IntBinaryOp::Add, x, k);
        let p = b.finish();
        assert_eq!(p.node_count(), 3);
        assert_eq!(p.output_count(), 3);
        // The root is derived as the unique sink (`add`).
        assert!(p.root().is_ok());
    }

    #[test]
    fn reachable_topo_orders_producers_before_consumers() {
        let mut b = MatcherBuilder::new();
        let a = b.leaf(crate::matcher::KindSpec::Any);
        let _unary = b.unary(crate::matcher::KindSpec::Any, a);
        let p = b.finish();
        let root = p.root().unwrap();
        let order = crate::graph_ext::reachable_topo(&p.graph, root).unwrap();
        // Two nodes; the producer precedes the consumer (root).
        assert_eq!(order.len(), 2);
        assert_eq!(*order.last().unwrap(), root);
    }
}
