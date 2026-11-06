use std::marker::PhantomData;

use cranelift_entity::EntityRef;

use entity_utils::DenseEntitySet;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WalkPhase {
    /// Entering the node; successors not yet visited.
    Pre,
    /// Leaving the node; all successors already visited.
    Post,
}

pub trait GraphRef {
    type NodeId: EntityRef;

    /// Calls `f` once per successor of `node`, in push order.
    fn successors(&self, node: Self::NodeId, f: impl FnMut(Self::NodeId));
}

impl<G: GraphRef> GraphRef for &'_ G {
    type NodeId = G::NodeId;

    fn successors(&self, node: Self::NodeId, f: impl FnMut(Self::NodeId)) {
        (*self).successors(node, f);
    }
}

/// The graph whose successors of a node are what `F` returns for it.
pub struct FnSuccs<N, F>(F, PhantomData<fn(N)>);

impl<N, F> FnSuccs<N, F> {
    pub fn new(f: F) -> Self {
        Self(f, PhantomData)
    }
}

impl<N: EntityRef, I: IntoIterator<Item = N>, F: Fn(N) -> I> GraphRef for FnSuccs<N, F> {
    type NodeId = N;

    fn successors(&self, node: N, mut f: impl FnMut(N)) {
        for succ in (self.0)(node) {
            f(succ);
        }
    }
}

/// Yields each node reachable from the roots once, in DFS preorder;
/// successors are visited in reverse push order.
pub struct PreOrder<G: GraphRef> {
    graph: G,
    visited: DenseEntitySet<G::NodeId>,
    stack: Vec<G::NodeId>,
}

impl<G: GraphRef> PreOrder<G> {
    /// Roots pop last-first.
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
            // Not a substitute for the pop-time check.
            if !visited.contains(succ) {
                stack.push(succ);
            }
        });

        Some(node)
    }
}

/// Yields each reached node once, after every successor that is not a DFS
/// ancestor of it.
pub struct PostOrder<G: GraphRef> {
    graph: G,
    visited: DenseEntitySet<G::NodeId>,
    stack: Vec<(WalkPhase, G::NodeId)>,
}

impl<G: GraphRef> PostOrder<G> {
    /// Roots pop last-first, so in a derived RPO an earlier root precedes
    /// every later root that does not reach it.
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

    /// The next pre- or post-visit event; the iterator yields post-visits only.
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
                            // Not a substitute for the pop-time check.
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
#[cfg(feature = "test-util")]
pub fn acyclic_postorder<G: GraphRef>(
    graph: G,
    roots: impl IntoIterator<Item = G::NodeId>,
) -> Result<Vec<G::NodeId>, G::NodeId> {
    let mut walk = PostOrder::new(&graph, roots);
    // Not a `DenseEntitySet`: dropping its maximum costs O(max_index / 64).
    let mut on_path: cranelift_entity::SecondaryMap<G::NodeId, bool> =
        cranelift_entity::SecondaryMap::new();
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
