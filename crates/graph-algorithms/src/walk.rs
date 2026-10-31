use alloc::vec::Vec;

use cranelift_entity::{EntityRef, SecondaryMap};

use entity_utils::set::DenseEntitySet;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WalkPhase {
    /// Entering the node; successors not yet visited.
    Pre,
    /// Leaving the node; all successors already visited.
    Post,
}

pub trait GraphRef {
    type NodeId: EntityRef;

    /// Calls `f` once per successor of `node`, in the order the walks push
    /// them. Implement it with a `for` loop: `for_each` over an adapter chain
    /// runs slower here.
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

impl<G: GraphRef> PreOrder<G> {
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

impl<G: GraphRef> Iterator for PreOrder<G> {
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

impl<G: GraphRef> PostOrder<G> {
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

impl<G: GraphRef> Iterator for PostOrder<G> {
    type Item = G::NodeId;

    fn next(&mut self) -> Option<G::NodeId> {
        loop {
            if let (WalkPhase::Post, node) = self.next_event()? {
                return Some(node);
            }
        }
    }
}

/// [`PostOrder`] of what `roots` reach, every node after its successors, or
/// `Err(node)` for the first node found with a successor on the current DFS
/// path, which puts it on a cycle. Asks each reached node for its successors
/// twice.
pub fn acyclic_postorder<G: GraphRef>(
    graph: G,
    roots: impl IntoIterator<Item = G::NodeId>,
) -> Result<Vec<G::NodeId>, G::NodeId> {
    let mut walk = PostOrder::new(&graph, roots);
    // Not a `DenseEntitySet`: dropping its maximum costs O(max_index / 64).
    let mut on_path: SecondaryMap<G::NodeId, bool> = SecondaryMap::new();
    let mut order = Vec::new();
    while let Some((phase, node)) = walk.next_event() {
        match phase {
            WalkPhase::Pre => {
                on_path[node] = true;
                let mut back = false;
                graph.successors(node, |succ| back |= on_path[succ]);
                if back {
                    return Err(node);
                }
            }
            WalkPhase::Post => {
                on_path[node] = false;
                order.push(node);
            }
        }
    }
    Ok(order)
}
