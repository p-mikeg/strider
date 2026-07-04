//! Generic depth-first graph traversal: pre-order and post-order walks over
//! any [`GraphRef`], with a pluggable [`VisitTracker`].  Parameterised over
//! opaque node ids, so no CFG/IR type is in scope here.

use alloc::vec::Vec;
use core::ops::ControlFlow;

use cranelift_entity::EntityRef;

use entity_utils::set::DenseEntitySet;

/// The visit phase reported by a post-order walk event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WalkPhase {
    /// The node is being entered; its successors have not yet been visited.
    Pre,
    /// The node is being left; all successors have already been visited.
    Post,
}

/// A directed graph that can enumerate the successors of any node.
///
/// Implement this trait to drive [`PreOrder`], [`PostOrder`], and other
/// traversals in this crate.
pub trait GraphRef {
    /// The node identifier type.
    type NodeId: Copy;

    /// Calls `f` with each successor of `node`, short-circuiting if `f`
    /// returns [`ControlFlow::Break`].
    fn try_successors(
        &self,
        node: Self::NodeId,
        f: impl FnMut(Self::NodeId) -> ControlFlow<()>,
    ) -> ControlFlow<()>;

    /// Convenience wrapper over [`try_successors`](GraphRef::try_successors)
    /// that ignores the `ControlFlow` return value.
    fn successors(&self, node: Self::NodeId, mut f: impl FnMut(Self::NodeId)) {
        let _ = self.try_successors(node, |succ| {
            f(succ);
            ControlFlow::Continue(())
        });
    }
}

impl<G: GraphRef> GraphRef for &'_ G {
    type NodeId = G::NodeId;

    fn try_successors(
        &self,
        node: Self::NodeId,
        f: impl FnMut(Self::NodeId) -> ControlFlow<()>,
    ) -> ControlFlow<()> {
        (*self).try_successors(node, f)
    }
}
/// Tracks which nodes have already been visited during a graph walk.
pub trait VisitTracker<N>: Default {
    /// Returns `true` if `node` has been marked as visited.
    fn is_visited(&self, node: N) -> bool;
    /// Marks `node` as visited.
    fn mark_visited(&mut self, node: N);
}

impl<N: EntityRef> VisitTracker<N> for DenseEntitySet<N> {
    fn is_visited(&self, node: N) -> bool {
        self.contains(node)
    }

    fn mark_visited(&mut self, node: N) {
        self.insert(node);
    }
}

/// Internal stack-based state for a pre-order DFS traversal.
#[derive(Debug)]
pub struct PreOrderContext<N> {
    stack: Vec<N>,
}

impl<N: Copy> PreOrderContext<N> {
    /// Creates an empty context.
    pub const fn new() -> Self {
        Self { stack: Vec::new() }
    }

    /// Resets the traversal, replacing the current stack with `roots`.
    ///
    /// **Root visit order:** because the internal stack is popped LIFO, roots are
    /// visited in **reverse** of their iteration order. That is, if `roots`
    /// yields `[u, v]` (with no path from `v` to `u` in the graph), the pre-order
    /// walk yields `v` (and its subtree) before `u`. This is the *opposite* of
    /// [`PostOrderContext::reset`], which is carefully shaped so source order is
    /// preserved in any RPO derived from a post-order walk.
    ///
    /// Callers that want forward source-order over multiple roots should
    /// reverse the iterator themselves (e.g. `roots.into_iter().rev()`).
    pub fn reset(&mut self, roots: impl IntoIterator<Item = N>) {
        self.stack.clear();
        self.stack.extend(roots);
    }

    /// Pops and returns the next unvisited node, pushing its successors.
    pub fn next(
        &mut self,
        graph: impl GraphRef<NodeId = N>,
        visited: &mut impl VisitTracker<N>,
    ) -> Option<N> {
        let node = loop {
            let node = self.stack.pop()?;
            if !visited.is_visited(node) {
                break node;
            }
        };

        visited.mark_visited(node);

        graph.successors(node, |succ| {
            // This extra check here is an optimization to avoid needlessly placing
            // an obviously-visited node on to the stack. Even if the node is not
            // visited now, it may be by the time it is popped off the stack later.
            if !visited.is_visited(succ) {
                self.stack.push(succ);
            }
        });

        Some(node)
    }
}

impl<N: Copy> Default for PreOrderContext<N> {
    fn default() -> Self {
        Self::new()
    }
}

/// Pre-order DFS iterator over a [`GraphRef`].
///
/// Each node is yielded exactly once, before its successors.
pub struct PreOrder<G: GraphRef, V> {
    /// The graph being walked.
    graph: G,
    /// Tracks which nodes have already been visited.
    visited: V,
    ctx: PreOrderContext<G::NodeId>,
}

impl<G: GraphRef, V: VisitTracker<G::NodeId>> PreOrder<G, V> {
    /// Creates a pre-order traversal starting from `roots`.
    pub fn new(graph: G, roots: impl IntoIterator<Item = G::NodeId>) -> Self {
        let mut ctx = PreOrderContext::new();
        ctx.reset(roots);
        Self {
            graph,
            visited: V::default(),
            ctx,
        }
    }

    /// Consumes the walk and returns the visited-set tracker.
    ///
    /// Typically called after `for_each` / iterator drain to recover the
    /// set of nodes the walk reached without re-collecting yielded ids.
    pub fn into_visited(self) -> V {
        self.visited
    }
}

impl<G: GraphRef, V: VisitTracker<G::NodeId>> Iterator for PreOrder<G, V> {
    type Item = G::NodeId;

    fn next(&mut self) -> Option<G::NodeId> {
        self.ctx.next(&self.graph, &mut self.visited)
    }
}

/// Convenience constructor for a pre-order walk that uses a
/// [`DenseEntitySet`] as its visited tracker.
pub fn entity_preorder<G: GraphRef>(
    graph: G,
    roots: impl IntoIterator<Item = G::NodeId>,
) -> PreOrder<G, DenseEntitySet<G::NodeId>>
where
    G::NodeId: EntityRef,
{
    PreOrder::new(graph, roots)
}

/// Internal stack-based state for a post-order DFS traversal.
#[derive(Debug)]
pub struct PostOrderContext<N> {
    stack: Vec<(WalkPhase, N)>,
}

impl<N: Copy> PostOrderContext<N> {
    /// Creates an empty context.
    pub const fn new() -> Self {
        Self { stack: Vec::new() }
    }

    /// Resets the traversal, replacing the current stack with `roots`.
    pub fn reset(&mut self, roots: impl IntoIterator<Item = N>) {
        self.stack.clear();

        // Note: push the roots onto the stack in source order so that this order is preserved in
        // any RPO. Specifically, we want to guarantee that if `u` precedes `v` in `roots` and there
        // isn't a path from `v` to `u` in the graph, then `u` will still precede `v` in any RPO
        // obtained from this graph walk. Pushing the nodes onto the stack in order guarantees this,
        // as it ensures that `v` is always visited before `u`.
        //
        // Some clients depend on this behavior: for example, the live-node RPO of a function graph
        // should always start with its entry node, and the topological sort performed during
        // scheduling is supposed to preserve block headers and terminators.
        self.stack
            .extend(roots.into_iter().map(|node| (WalkPhase::Pre, node)));
    }

    /// Returns the next node in post-order (all successors already visited),
    /// or `None` when the traversal is complete.
    pub fn next(
        &mut self,
        graph: impl GraphRef<NodeId = N>,
        visited: &mut impl VisitTracker<N>,
    ) -> Option<N> {
        loop {
            let (phase, node) = self.next_event(&graph, visited)?;
            if phase == WalkPhase::Post {
                return Some(node);
            }
        }
    }

    /// Returns the next raw walk event `(WalkPhase, node)`, exposing both
    /// pre- and post-visit events to the caller.
    pub fn next_event(
        &mut self,
        graph: impl GraphRef<NodeId = N>,
        visited: &mut impl VisitTracker<N>,
    ) -> Option<(WalkPhase, N)> {
        loop {
            let (phase, node) = self.stack.pop()?;
            match phase {
                WalkPhase::Pre => {
                    if !visited.is_visited(node) {
                        visited.mark_visited(node);
                        self.stack.push((WalkPhase::Post, node));
                        graph.successors(node, |succ| {
                            // This extra check here is an optimization to avoid needlessly placing
                            // an obviously-visited node on to the stack. Even if the node is not
                            // visited now, it may be by the time it is popped off the stack later.
                            if !visited.is_visited(succ) {
                                self.stack.push((WalkPhase::Pre, succ));
                            }
                        });

                        return Some((WalkPhase::Pre, node));
                    }
                }
                WalkPhase::Post => {
                    return Some((WalkPhase::Post, node));
                }
            }
        }
    }
}

impl<N: Copy> Default for PostOrderContext<N> {
    fn default() -> Self {
        Self::new()
    }
}

/// Post-order DFS iterator over a [`GraphRef`].
///
/// Each node is yielded exactly once, after all of its successors.
pub struct PostOrder<G: GraphRef, V> {
    /// The graph being walked.
    graph: G,
    /// Tracks which nodes have already been visited.
    visited: V,
    ctx: PostOrderContext<G::NodeId>,
}

impl<G: GraphRef, V: VisitTracker<G::NodeId>> PostOrder<G, V> {
    /// Creates a post-order traversal starting from `roots`.
    pub fn new(graph: G, roots: impl IntoIterator<Item = G::NodeId>) -> Self {
        let mut ctx = PostOrderContext::new();
        ctx.reset(roots);
        Self {
            graph,
            visited: V::default(),
            ctx,
        }
    }

    /// Returns the next raw walk event; see
    /// [`PostOrderContext::next_event`] for details.
    pub fn next_event(&mut self) -> Option<(WalkPhase, G::NodeId)> {
        self.ctx.next_event(&self.graph, &mut self.visited)
    }

    /// Consumes the walk and returns the visited-set tracker.
    ///
    /// Typically called after `for_each` / iterator drain to recover the
    /// set of nodes the walk reached without re-collecting yielded ids.
    /// Mirrors [`PreOrder::into_visited`].
    pub fn into_visited(self) -> V {
        self.visited
    }
}

impl<G: GraphRef, V: VisitTracker<G::NodeId>> Iterator for PostOrder<G, V> {
    type Item = G::NodeId;

    fn next(&mut self) -> Option<G::NodeId> {
        self.ctx.next(&self.graph, &mut self.visited)
    }
}

/// Convenience constructor for a post-order walk that uses a
/// [`DenseEntitySet`] as its visited tracker.
pub fn entity_postorder<G: GraphRef>(
    graph: G,
    roots: impl IntoIterator<Item = G::NodeId>,
) -> PostOrder<G, DenseEntitySet<G::NodeId>>
where
    G::NodeId: EntityRef,
{
    PostOrder::new(graph, roots)
}
