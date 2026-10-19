use alloc::vec::Vec;

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

    fn successors(&self, node: Self::NodeId, f: impl FnMut(Self::NodeId));
}

impl<G: GraphRef> GraphRef for &'_ G {
    type NodeId = G::NodeId;

    fn successors(&self, node: Self::NodeId, f: impl FnMut(Self::NodeId)) {
        (*self).successors(node, f);
    }
}

/// Yields each node reachable from the roots exactly once. A DFS preorder,
/// which is NOT a topological order: successors are explored LIFO, so at a
/// join the node is emitted from the successor explored first and precedes the
/// earlier sibling that also reaches it.
pub struct PreOrder<G: GraphRef> {
    graph: G,
    visited: DenseEntitySet<G::NodeId>,
    stack: Vec<G::NodeId>,
}

impl<G: GraphRef> PreOrder<G>
where
    G::NodeId: EntityRef,
{
    /// Roots pop last-first, so a root no later root repeats or reaches is
    /// visited after every later root. [`PostOrder::new`] seeds its stack the
    /// same way.
    pub fn new(graph: G, roots: impl IntoIterator<Item = G::NodeId>) -> Self {
        Self {
            graph,
            visited: DenseEntitySet::default(),
            stack: roots.into_iter().collect(),
        }
    }

    /// The reached-node set, once the walk is drained.
    pub fn into_visited(self) -> DenseEntitySet<G::NodeId> {
        self.visited
    }
}

impl<G: GraphRef> Iterator for PreOrder<G>
where
    G::NodeId: EntityRef,
{
    type Item = G::NodeId;

    fn next(&mut self) -> Option<G::NodeId> {
        let Self {
            graph,
            visited,
            stack,
        } = self;
        let node = loop {
            let node = stack.pop()?;
            if !visited.contains(node) {
                break node;
            }
        };

        visited.insert(node);

        graph.successors(node, |succ| {
            // Keeps the stack small, but does not replace the pop-time check:
            // a node unvisited now may be visited by the time it is popped.
            if !visited.contains(succ) {
                stack.push(succ);
            }
        });

        Some(node)
    }
}

/// Yields each node exactly once, after every successor that is not a DFS
/// ancestor of it. On a cycle the back edge's target is already on the stack,
/// so it is yielded after the successor that closes the cycle.
pub struct PostOrder<G: GraphRef> {
    graph: G,
    visited: DenseEntitySet<G::NodeId>,
    stack: Vec<(WalkPhase, G::NodeId)>,
}

impl<G: GraphRef> PostOrder<G>
where
    G::NodeId: EntityRef,
{
    /// Roots go on the stack in source order and pop last-first, so a root no
    /// later root repeats or reaches precedes every later root in any derived
    /// RPO.
    pub fn new(graph: G, roots: impl IntoIterator<Item = G::NodeId>) -> Self {
        Self {
            graph,
            visited: DenseEntitySet::default(),
            stack: roots
                .into_iter()
                .map(|node| (WalkPhase::Pre, node))
                .collect(),
        }
    }

    /// Both the pre- and post-visit event of each node, where the iterator
    /// yields only the post-visits. The pre/post pair brackets the node's
    /// subtree, which is what bounds the current DFS path.
    pub fn next_event(&mut self) -> Option<(WalkPhase, G::NodeId)> {
        let Self {
            graph,
            visited,
            stack,
        } = self;
        loop {
            let (phase, node) = stack.pop()?;
            match phase {
                WalkPhase::Pre => {
                    if !visited.contains(node) {
                        visited.insert(node);
                        stack.push((WalkPhase::Post, node));
                        graph.successors(node, |succ| {
                            // Keeps the stack small, but does not replace the
                            // pop-time check: a node unvisited now may be
                            // visited by the time it is popped.
                            if !visited.contains(succ) {
                                stack.push((WalkPhase::Pre, succ));
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

impl<G: GraphRef> Iterator for PostOrder<G>
where
    G::NodeId: EntityRef,
{
    type Item = G::NodeId;

    fn next(&mut self) -> Option<G::NodeId> {
        loop {
            if let (WalkPhase::Post, node) = self.next_event()? {
                return Some(node);
            }
        }
    }
}
