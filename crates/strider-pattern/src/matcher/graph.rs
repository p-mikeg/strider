use rustc_hash::{FxHashMap, FxHashSet};
use strider_graph::{Graph, NeverCacheable, NodeId};

use super::CastMask;
use super::vertex::{PatNode, PatValue, PostMatchFn};
use crate::graph_ext::PatGraphRead;

pub(crate) type PatGraph = Graph<PatNode, PatValue, NeverCacheable>;

pub struct Pattern {
    pub(crate) graph: PatGraph,
    pub(crate) cast_mask: CastMask,
    /// Resolved once at seal and memoized, verdict included.
    root: Result<NodeId, String>,
    /// Per pat node, indexed by `NodeId::as_u32`.
    inputs: Vec<super::walk::NodeInputs>,
}

impl Pattern {
    /// Seal point of [`MatcherBuilder`](crate::matcher::MatcherBuilder):
    /// resolves and memoizes the match root.
    pub(crate) fn from_graph(graph: PatGraph) -> Self {
        let root = Self::resolve_root(&graph).map_err(|e| e.to_string());
        let inputs = super::walk::collect_node_inputs(&graph);
        Self {
            graph,
            cast_mask: CastMask::empty(),
            root,
            inputs,
        }
    }

    /// The structure is frozen at seal, so a match attempt reads this instead
    /// of rebuilding the edge list per operand ordering.
    pub(crate) fn inputs_of(&self, node: NodeId) -> &super::walk::NodeInputs {
        &self.inputs[node.as_u32() as usize]
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
    /// If the pattern is rootless, cyclic, or multi-sink.
    pub fn root(&self) -> anyhow::Result<NodeId> {
        self.root.clone().map_err(anyhow::Error::msg)
    }

    /// Includes what a node's binding walk declares: that sub-pattern's graph
    /// lives inside the walk closure, not here.
    pub fn bound_captures(&self) -> impl Iterator<Item = crate::capture::Capture> + '_ {
        self.graph
            .all_node_ids()
            .filter_map(|n| self.graph.node_kind(n).capture)
            .chain(
                self.graph
                    .all_value_ids()
                    .filter_map(|v| self.graph.value_kind_ref(v).capture),
            )
            .chain(
                self.graph
                    .all_node_ids()
                    .flat_map(|n| self.graph.node_kind(n).walk_captures.bound.iter().copied()),
            )
    }

    /// The captures bound on EVERY successful match, as opposed to
    /// [`Self::bound_captures`], which reports every capture appearing anywhere
    /// in the graph.
    ///
    /// The two differ only under an alternation: `one_of` binds whichever arm
    /// fires, so a capture present in some arms but not all is not guaranteed.
    ///
    /// # Errors
    /// If the pattern is rootless, cyclic, or multi-sink.
    pub fn guaranteed_captures(&self) -> anyhow::Result<FxHashSet<crate::capture::Capture>> {
        let mut memo: FxHashMap<NodeId, FxHashSet<crate::capture::Capture>> = FxHashMap::default();
        Ok(self.guaranteed_from(self.root()?, &mut memo))
    }

    /// The pattern graph is acyclic (`resolve_root` proves it), so the memo
    /// makes this linear and the recursion terminates.
    fn guaranteed_from(
        &self,
        node: NodeId,
        memo: &mut FxHashMap<NodeId, FxHashSet<crate::capture::Capture>>,
    ) -> FxHashSet<crate::capture::Capture> {
        if let Some(hit) = memo.get(&node) {
            return hit.clone();
        }
        let mut out: FxHashSet<crate::capture::Capture> = FxHashSet::default();
        // This node's own capture, and any on the values it produces, bind
        // whenever the node matches at all. A binding walk has to succeed for
        // the node to match, so what it guarantees is guaranteed here.
        if let Some(c) = self.graph.node_kind(node).capture {
            out.insert(c);
        }
        out.extend(
            self.graph
                .node_kind(node)
                .walk_captures
                .guaranteed
                .iter()
                .copied(),
        );
        for &vertex in self.graph.node_outputs(node) {
            if let Some(c) = self.graph.value_kind_ref(vertex).capture {
                out.insert(c);
            }
        }
        // Each input contributes its own vertex capture plus everything its
        // producer guarantees. For an alternation those are ARMS, so the vertex
        // capture belongs to that arm and must not be hoisted out of the
        // intersection.
        let per_input: Vec<FxHashSet<crate::capture::Capture>> = self
            .graph
            .consumed_inputs(node)
            .into_iter()
            .map(|(_, vertex)| {
                let mut caps = self.guaranteed_from(self.graph.producer_of(vertex), memo);
                if let Some(c) = self.graph.value_kind_ref(vertex).capture {
                    caps.insert(c);
                }
                caps
            })
            .collect();
        if self.graph.node_kind(node).alternation {
            // Exactly one arm fires, so only what EVERY arm binds is guaranteed.
            let mut arms = per_input.into_iter();
            if let Some(first) = arms.next() {
                let common = arms.fold(first, |acc, arm| acc.intersection(&arm).copied().collect());
                out.extend(common);
            }
        } else {
            // Every operand must match, so all of their captures bind.
            for caps in per_input {
                out.extend(caps);
            }
        }
        memo.insert(node, out.clone());
        out
    }

    /// Runs after root and all inputs have matched; returning `false` rejects
    /// the match. Composes with a guard already on the root, like
    /// [`MatcherBuilder::set_post_match`](crate::matcher::MatcherBuilder::set_post_match).
    ///
    /// # Panics
    ///
    /// If the pattern has no unique sink root.
    #[allow(clippy::expect_used)]
    pub(crate) fn set_root_post_match(&mut self, f: PostMatchFn) {
        let root = self
            .graph
            .derive_root()
            .expect("pattern has a unique sink root");
        let slot = &mut self.graph.node_kind_mut(root).post_match;
        *slot = Some(match slot.take() {
            Some(prev) => Box::new(move |m, n, ty, b| prev(m, n, ty, b) && f(m, n, ty, b)),
            None => f,
        });
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
        // Root is the unique sink (`int_add`).
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
