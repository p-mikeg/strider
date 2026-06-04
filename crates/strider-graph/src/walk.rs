//! Structural graph walks — payload-agnostic, no control-flow concept.
//!
//! The IR's `reverse_postorder` (in `strider-ir`'s `walk` module) mixes a
//! backward-data + forward-control reachability relation with a forward def→use
//! post-order. The forward-control half is strider-specific (it consults
//! `ValueKind::is_control`), so the generic crate ports ONLY the structural
//! def→use part:
//!
//! - [`preorder`] — input-following reachability from a set of seeds (the
//!   backward-data closure; every producer of every reachable node's inputs).
//! - [`reverse_postorder`] — a true def→use RPO over that reachable cone:
//!   every producer is yielded strictly before its consumers, input-less roots
//!   first. Built as a post-order over the forward def→use edges restricted to
//!   the reachable set, then reversed.

use cranelift_entity::SecondaryMap;

use crate::cache::NodeCacheable;
use crate::graph::Graph;
use crate::ids::NodeId;

impl<N, V, C: NodeCacheable<N, V>> Graph<N, V, C> {
    /// Input-following preorder reachability from `seeds`.
    ///
    /// Visits every node reachable by walking input-producer edges backward
    /// (defs of a node's inputs, transitively). Each reachable node appears
    /// exactly once; the order is preorder relative to the backward walk and
    /// is not a topological guarantee — use [`Self::reverse_postorder`] for
    /// defs-before-uses ordering.
    pub fn preorder(&self, seeds: impl IntoIterator<Item = NodeId>) -> Vec<NodeId> {
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
    /// roots (restricted to the reachable set), and reverse.
    pub fn reverse_postorder(&self, seeds: impl IntoIterator<Item = NodeId>) -> Vec<NodeId> {
        // 1. Reachable set + input-less roots.
        let reachable_order = self.preorder(seeds);
        let mut reachable: SecondaryMap<NodeId, bool> = SecondaryMap::new();
        let mut roots: Vec<NodeId> = Vec::new();
        for &node in &reachable_order {
            reachable[node] = true;
            if self.node_inputs(node).is_empty() {
                roots.push(node);
            }
        }

        // 2. Iterative post-order over forward def→use edges, restricted to
        // the reachable set. `on_stack` guards against re-pushing a node
        // already being expanded (cycle safety); `done` records completion.
        let mut done: SecondaryMap<NodeId, bool> = SecondaryMap::new();
        let mut on_stack: SecondaryMap<NodeId, bool> = SecondaryMap::new();
        let mut postorder: Vec<NodeId> = Vec::new();
        // Each stack frame: (node, has_expanded_children).
        let mut stack: Vec<(NodeId, bool)> = Vec::new();

        for &root in &roots {
            if done[root] {
                continue;
            }
            stack.push((root, false));
            on_stack[root] = true;
            while let Some((node, expanded)) = stack.pop() {
                if expanded {
                    done[node] = true;
                    on_stack[node] = false;
                    postorder.push(node);
                    continue;
                }
                if done[node] {
                    continue;
                }
                // Re-push the node marked as expanded; its children go on top.
                stack.push((node, true));
                for consumer in self.def_use_consumers(node) {
                    if reachable[consumer] && !done[consumer] && !on_stack[consumer] {
                        on_stack[consumer] = true;
                        stack.push((consumer, false));
                    }
                }
            }
        }

        // Defensive: any reachable node not yet emitted (e.g. only reachable
        // through a cycle with no input-less root) gets a post-order pass too.
        for &node in &reachable_order {
            if !done[node] {
                stack.push((node, false));
                on_stack[node] = true;
                while let Some((n, expanded)) = stack.pop() {
                    if expanded {
                        done[n] = true;
                        on_stack[n] = false;
                        postorder.push(n);
                        continue;
                    }
                    if done[n] {
                        continue;
                    }
                    stack.push((n, true));
                    for consumer in self.def_use_consumers(n) {
                        if reachable[consumer] && !done[consumer] && !on_stack[consumer] {
                            on_stack[consumer] = true;
                            stack.push((consumer, false));
                        }
                    }
                }
            }
        }

        postorder.reverse();
        postorder
    }

    /// Every distinct node that consumes one of `node`'s outputs.
    fn def_use_consumers(&self, node: NodeId) -> Vec<NodeId> {
        let mut seen: SecondaryMap<NodeId, bool> = SecondaryMap::new();
        let mut out: Vec<NodeId> = Vec::new();
        for &output in self.node_outputs(node) {
            for (consumer, _) in self.value_uses(output) {
                if !seen[consumer] {
                    seen[consumer] = true;
                    out.push(consumer);
                }
            }
        }
        out
    }
}
