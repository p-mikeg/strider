//! Structural graph walks — payload-agnostic, no control-flow concept.
//!
//! The IR's `reverse_postorder` (in `strider-ir`'s `walk` module) mixes a
//! backward-data + forward-control reachability relation with a forward def→use
//! post-order. The forward-control half is strider-specific (it consults
//! `ValueKind::is_control`), so the generic crate ports ONLY the structural
//! def→use part:
//!
//! - [`Graph::preorder_seeds`] — input-following reachability from a set of
//!   seeds (the backward-data closure; every producer of every reachable node's
//!   inputs).
//! - [`Graph::reverse_postorder_seeds`] — a true def→use RPO over that reachable
//!   cone: every producer is yielded strictly before its consumers, input-less
//!   roots first.
//!
//! [`Graph::reverse_postorder_seeds`] is built exactly like `strider-ir`'s RPO (see
//! `crates/strider-ir/src/walk/mod.rs`): a [`graphwalk::PostOrder`] over the
//! forward def→use successor relation (each node's successors are the nodes
//! that consume its outputs), seeded from the input-less roots and reversed.
//! `graphwalk::PostOrder` handles cycles with a single visited set, so there is
//! no bespoke `on_stack` guard or cleanup pass.

use core::ops::ControlFlow;

use cranelift_entity::SecondaryMap;
use graphwalk::{GraphRef, PostOrder};

use crate::cache::NodeCacheable;
use crate::graph::Graph;
use crate::ids::NodeId;

/// A [`graphwalk::GraphRef`] over the forward def→use edges of a [`Graph`],
/// restricted to a precomputed reachable set.
///
/// A node's successors are every distinct node that consumes one of its
/// outputs. Driving a post-order with this relation yields every node after all
/// of its consumers, so reversing the post-order gives a true RPO (every
/// producer strictly before its consumers).
struct DefUseSuccs<'a, N, V, C: NodeCacheable<N, V>> {
    graph: &'a Graph<N, V, C>,
    reachable: &'a SecondaryMap<NodeId, bool>,
}

impl<N, V, C: NodeCacheable<N, V>> GraphRef for DefUseSuccs<'_, N, V, C> {
    type NodeId = NodeId;

    fn try_successors(
        &self,
        node: NodeId,
        mut f: impl FnMut(NodeId) -> ControlFlow<()>,
    ) -> ControlFlow<()> {
        let mut seen: SecondaryMap<NodeId, bool> = SecondaryMap::new();
        for &output in self.graph.node_outputs(node) {
            for (consumer, _) in self.graph.value_uses(output) {
                if self.reachable[consumer] && !seen[consumer] {
                    seen[consumer] = true;
                    f(consumer)?;
                }
            }
        }
        ControlFlow::Continue(())
    }
}

impl<N, V, C: NodeCacheable<N, V>> Graph<N, V, C> {
    /// Input-following preorder reachability from `seeds`.
    ///
    /// Visits every node reachable by walking input-producer edges backward
    /// (defs of a node's inputs, transitively). Each reachable node appears
    /// exactly once; the order is preorder relative to the backward walk and
    /// is not a topological guarantee — use [`Self::reverse_postorder_seeds`] for
    /// defs-before-uses ordering.
    pub fn preorder_seeds(&self, seeds: impl IntoIterator<Item = NodeId>) -> Vec<NodeId> {
        let mut visited: SecondaryMap<NodeId, bool> = SecondaryMap::new();
        let mut order: Vec<NodeId> = Vec::new();
        let mut stack: Vec<NodeId> = seeds.into_iter().collect();
        while let Some(node) = stack.pop() {
            if visited[node] {
                continue;
            }
            visited[node] = true;
            order.push(node);
            for input in self.node_inputs(node) {
                stack.push(self.producer(input));
            }
        }
        order
    }

    /// True reverse-post-order over the def→use cone reachable from `seeds`.
    ///
    /// Every producer is yielded strictly before each of its consumers, with
    /// input-less roots first. Cycles terminate (each node is visited once).
    ///
    /// Implementation: discover the reachable set + its input-less roots via a
    /// backward input walk, post-order the forward def→use graph from those
    /// roots (restricted to the reachable set) with [`graphwalk::PostOrder`],
    /// and reverse.
    pub fn reverse_postorder_seeds(&self, seeds: impl IntoIterator<Item = NodeId>) -> Vec<NodeId> {
        // 1. Reachable set + input-less roots.
        let reachable_order = self.preorder_seeds(seeds);
        let mut reachable: SecondaryMap<NodeId, bool> = SecondaryMap::new();
        let mut roots: Vec<NodeId> = Vec::new();
        for &node in &reachable_order {
            reachable[node] = true;
            if self.node_inputs(node).is_empty() {
                roots.push(node);
            }
        }

        // 2. Post-order over the forward def→use relation from the roots,
        // restricted to the reachable set. `graphwalk::PostOrder` carries a
        // single visited set, so cycles terminate without an `on_stack` guard.
        let succs = DefUseSuccs {
            graph: self,
            reachable: &reachable,
        };
        let mut postorder: Vec<NodeId> =
            PostOrder::<_, SecondaryMapTracker>::new(succs, roots.iter().copied()).collect();

        postorder.reverse();
        postorder
    }
}

/// A [`graphwalk::VisitTracker`] backed by a `SecondaryMap<NodeId, bool>`, so
/// the post-order walk does not require `NodeId: EntityRef` plumbing beyond
/// what `cranelift-entity` already provides here.
#[derive(Default)]
struct SecondaryMapTracker(SecondaryMap<NodeId, bool>);

impl graphwalk::VisitTracker<NodeId> for SecondaryMapTracker {
    fn is_visited(&self, node: NodeId) -> bool {
        self.0[node]
    }

    fn mark_visited(&mut self, node: NodeId) {
        self.0[node] = true;
    }
}

#[cfg(test)]
mod tests {
    use smallvec::SmallVec;

    use crate::cache::NeverCacheable;
    use crate::graph::Graph;
    use crate::ids::ValueId;

    type TestGraph = Graph<&'static str, (), NeverCacheable>;

    fn node(g: &mut TestGraph, kind: &'static str, inputs: &[ValueId]) -> ValueId {
        let n = g.create_node(kind, inputs.iter().copied(), [()]);
        g.node_outputs(n)[0]
    }

    /// A diamond DAG: `a` is an input-less root; `b`,`c` depend on `a`; `d`
    /// depends on `b`,`c`. RPO must yield every producer before its consumers.
    #[test]
    fn reverse_postorder_yields_defs_before_uses_on_a_diamond() {
        let mut g = TestGraph::new();
        let a = node(&mut g, "a", &[]);
        let b = node(&mut g, "b", &[a]);
        let c = node(&mut g, "c", &[a]);
        let d = node(&mut g, "d", &[b, c]);

        let (na, nb, nc, nd) = (
            g.producer(a),
            g.producer(b),
            g.producer(c),
            g.producer(d),
        );

        let order = g.reverse_postorder_seeds([nd]);
        assert_eq!(order.len(), 4, "each cone node once: {order:?}");
        let pos = |n| order.iter().position(|&x| x == n).unwrap();
        assert!(pos(na) < pos(nb), "a before b: {order:?}");
        assert!(pos(na) < pos(nc), "a before c: {order:?}");
        assert!(pos(nb) < pos(nd), "b before d: {order:?}");
        assert!(pos(nc) < pos(nd), "c before d: {order:?}");
        // Root first, sole sink last.
        assert_eq!(order.first(), Some(&na), "input-less root first: {order:?}");
        assert_eq!(order.last(), Some(&nd), "sole sink last: {order:?}");
    }

    /// A shared operand is visited exactly once, before its consumer.
    #[test]
    fn reverse_postorder_visits_shared_operand_once() {
        let mut g = TestGraph::new();
        let k = node(&mut g, "k", &[]);
        let add = g.create_node("add", [k, k], [()]);
        let order = g.reverse_postorder_seeds([add]);
        assert_eq!(order, vec![g.producer(k), add], "shared operand once, before consumer");
    }

    /// `preorder` reaches every backward-reachable producer.
    #[test]
    fn preorder_reaches_all_producers() {
        let mut g = TestGraph::new();
        let a = node(&mut g, "a", &[]);
        let b = node(&mut g, "b", &[a]);
        let d_val = node(&mut g, "d", &[b]);
        let d = g.producer(d_val);
        let reached: SmallVec<[_; 4]> = g.preorder_seeds([d]).into();
        assert!(reached.contains(&g.producer(a)));
        assert!(reached.contains(&g.producer(b)));
        assert!(reached.contains(&d));
    }
}
