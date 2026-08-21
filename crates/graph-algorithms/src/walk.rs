use alloc::vec::Vec;
use core::ops::ControlFlow;

use cranelift_entity::EntityRef;

use entity_utils::set::DenseEntitySet;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WalkPhase {
    /// Entering the node; successors not yet visited.
    Pre,
    /// Leaving the node; all successors already visited.
    Post,
}

pub trait GraphRef {
    type NodeId: Copy;

    /// Short-circuits if `f` returns [`ControlFlow::Break`].
    fn try_successors(
        &self,
        node: Self::NodeId,
        f: impl FnMut(Self::NodeId) -> ControlFlow<()>,
    ) -> ControlFlow<()>;

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
pub trait VisitTracker<N>: Default {
    fn is_visited(&self, node: N) -> bool;
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

/// Stack state for a pre-order DFS, split out so callers can drive a walk
/// without owning the graph.
#[derive(Debug)]
pub struct PreOrderContext<N> {
    stack: Vec<N>,
}

impl<N: Copy> PreOrderContext<N> {
    pub const fn new() -> Self {
        Self { stack: Vec::new() }
    }

    /// Roots are visited in REVERSE iteration order, since the stack pops
    /// LIFO. That is the opposite of [`PostOrderContext::reset`], shaped to
    /// preserve source order in a derived RPO. Callers wanting forward order
    /// here must reverse the iterator themselves.
    pub fn reset(&mut self, roots: impl IntoIterator<Item = N>) {
        self.stack.clear();
        self.stack.extend(roots);
    }

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
            // Keeps the stack small, but does not replace the pop-time check:
            // a node unvisited now may be visited by the time it is popped.
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

/// Yields each node exactly once, before its successors.
pub struct PreOrder<G: GraphRef, V> {
    graph: G,
    visited: V,
    ctx: PreOrderContext<G::NodeId>,
}

impl<G: GraphRef, V: VisitTracker<G::NodeId>> PreOrder<G, V> {
    pub fn new(graph: G, roots: impl IntoIterator<Item = G::NodeId>) -> Self {
        let mut ctx = PreOrderContext::new();
        ctx.reset(roots);
        Self {
            graph,
            visited: V::default(),
            ctx,
        }
    }

    /// The reached-node set, once the walk is drained.
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

pub fn entity_preorder<G: GraphRef>(
    graph: G,
    roots: impl IntoIterator<Item = G::NodeId>,
) -> PreOrder<G, DenseEntitySet<G::NodeId>>
where
    G::NodeId: EntityRef,
{
    PreOrder::new(graph, roots)
}

/// Stack state for a post-order DFS, split out so callers can drive a walk
/// without owning the graph.
#[derive(Debug)]
pub struct PostOrderContext<N> {
    stack: Vec<(WalkPhase, N)>,
}

impl<N: Copy> PostOrderContext<N> {
    pub const fn new() -> Self {
        Self { stack: Vec::new() }
    }

    /// Roots go on the stack in source order, which (LIFO) visits them
    /// backwards and so preserves source order in any derived RPO: if `u`
    /// precedes `v` in `roots` and no path runs `v -> u`, `u` precedes `v` in
    /// the RPO.
    pub fn reset(&mut self, roots: impl IntoIterator<Item = N>) {
        self.stack.clear();
        self.stack
            .extend(roots.into_iter().map(|node| (WalkPhase::Pre, node)));
    }

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

    /// Exposes both pre- and post-visit events; [`next`](Self::next) filters
    /// down to the post-visits.
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
                            // Keeps the stack small, but does not replace the
                            // pop-time check: a node unvisited now may be
                            // visited by the time it is popped.
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

/// Yields each node exactly once, after all of its successors.
pub struct PostOrder<G: GraphRef, V> {
    graph: G,
    visited: V,
    ctx: PostOrderContext<G::NodeId>,
}

impl<G: GraphRef, V: VisitTracker<G::NodeId>> PostOrder<G, V> {
    pub fn new(graph: G, roots: impl IntoIterator<Item = G::NodeId>) -> Self {
        let mut ctx = PostOrderContext::new();
        ctx.reset(roots);
        Self {
            graph,
            visited: V::default(),
            ctx,
        }
    }

    /// See [`PostOrderContext::next_event`].
    pub fn next_event(&mut self) -> Option<(WalkPhase, G::NodeId)> {
        self.ctx.next_event(&self.graph, &mut self.visited)
    }

    /// The reached-node set, once the walk is drained.
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

pub fn entity_postorder<G: GraphRef>(
    graph: G,
    roots: impl IntoIterator<Item = G::NodeId>,
) -> PostOrder<G, DenseEntitySet<G::NodeId>>
where
    G::NodeId: EntityRef,
{
    PostOrder::new(graph, roots)
}
