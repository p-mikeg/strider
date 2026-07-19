//! The bipartite match pattern: a [`strider_graph::Graph`] over [`PatNode`] /
//! [`PatValue`] under the always-allocate
//! [`NeverCacheable`](strider_graph::NeverCacheable) policy.
//!
//! The read vocabulary the matcher needs (`consumed_inputs` with per-edge
//! slots, `derive_root`, ...) is added by [`crate::graph_ext`]; construction
//! lives in [`MatcherBuilder`](crate::matcher::MatcherBuilder), which stages
//! nodes and materialises them at seal time.

use strider_graph::{Graph, NeverCacheable, NodeId};

use super::CastMask;
use super::vertex::{PatNode, PatValue, PostMatchFn};
use crate::graph_ext::PatGraphRead;

pub(crate) type PatGraph = Graph<PatNode, PatValue, NeverCacheable>;

pub struct Pattern {
    pub(crate) graph: PatGraph,
    pub(crate) cast_mask: CastMask,
    /// Resolved once at seal and memoized, verdict included: caching the
    /// `Err` too keeps [`Matcher::match_at`](crate::Matcher::match_at) from
    /// re-deriving the root and re-walking for acyclicity on every candidate
    /// node the rewrite driver probes.
    root: Result<NodeId, String>,
}

impl Pattern {
    /// Seal point of [`MatcherBuilder`]. The graph structure is fixed from
    /// here on, so the match root is resolved and memoized now.
    pub(crate) fn from_graph(graph: PatGraph) -> Self {
        let root = Self::resolve_root(&graph).map_err(|e| e.to_string());
        Self {
            graph,
            cast_mask: CastMask::empty(),
            root,
        }
    }

    /// The unique sink, after confirming its input cone is acyclic.
    ///
    /// # Errors
    /// Zero sinks (rootless / cyclic), more than one sink, or a cycle in the
    /// root's input cone.
    fn resolve_root(graph: &PatGraph) -> anyhow::Result<NodeId> {
        let root = graph.derive_root()?;
        crate::graph_ext::reachable_topo(graph, root)?;
        Ok(root)
    }

    /// O(1) read of the verdict memoized at construction.
    ///
    /// # Errors
    /// The recorded error if the pattern is rootless, cyclic, or multi-sink.
    /// A multi-sink graph is legal to build (shared captures produce one) but
    /// not matchable.
    pub fn root(&self) -> anyhow::Result<NodeId> {
        self.root.clone().map_err(anyhow::Error::msg)
    }

    /// Every [`Capture`](crate::capture::Capture) this pattern binds. Value
    /// captures live on the producing output vertex, value-less roots on the
    /// node; both are collected.
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

    /// Runs after root and all inputs have matched; returning `false` rejects
    /// the match. Control / variadic builders finalise straight to a `Pattern`
    /// with no value-output `MatchPat` form to wrap, so this is the only way
    /// to give them a root guard.
    ///
    /// # Panics
    ///
    /// If the pattern has no unique sink root, which a finished pattern
    /// always has.
    #[allow(clippy::expect_used)]
    pub(crate) fn set_root_post_match(&mut self, f: PostMatchFn) {
        let root = self
            .graph
            .derive_root()
            .expect("pattern has a unique sink root");
        self.graph.node_kind_mut(root).post_match = Some(f);
    }

    /// Builder form of `set_root_post_match`.
    pub fn with_root_post_match(mut self, f: PostMatchFn) -> Self {
        self.set_root_post_match(f);
        self
    }

    /// Adds `m` to the cast-walk-through mask; the matcher then skips the
    /// selected cast kinds transparently.
    pub fn ignore_casts_mask(mut self, m: CastMask) -> Self {
        self.cast_mask |= m;
        self
    }

    /// Walk through every value-passthrough cast kind.
    pub fn ignore_casts(self) -> Self {
        self.ignore_casts_mask(CastMask::all())
    }

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
    use strider_ir::ConstId;
    use strider_ir::node::NodeKind;

    #[test]
    fn builds_bipartite_add_shape() {
        let mut b = MatcherBuilder::new();
        let x = b.leaf(crate::matcher::KindSpec::Any);
        let k = b.leaf(crate::matcher::KindSpec::Exact(NodeKind::IntConst(
            ConstId::from_u32(1),
        )));
        let _sum = b.binary(strider_ir::IntBinaryOp::Add, x, k);
        let p = b.finish();
        assert_eq!(p.graph.all_node_ids().count(), 3);
        assert_eq!(p.graph.all_value_ids().count(), 3);
        // Root is the unique sink (`add`).
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
        assert_eq!(order.len(), 2);
        assert_eq!(*order.last().unwrap(), root);
    }
}
