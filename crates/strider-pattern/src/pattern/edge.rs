//! Edge weights for the bipartite pattern graph.

/// A directed edge in the bipartite pattern graph.
///
/// `Produces` runs from a producer [`PatNode`](super::PatNode) to one
/// of its [`PatOutput`](super::PatOutput) vertices; `Consumes` runs
/// from a `PatOutput` to a consuming `PatNode`, recording which input
/// slot of the consumer the output feeds.
#[derive(Clone, Copy, Debug)]
pub enum PatEdge {
    /// Producer node → its output vertex.
    Produces,
    /// Output vertex → consuming node at the given input slot.
    Consumes { slot: usize },
}
