//! Dense integer identifiers for nodes, outputs, and inputs.
//!
//! Each id is a small integer allocated from a [`cranelift_entity::PrimaryMap`]
//! inside the graph's [`crate::storage::RawStore`], so they can be passed
//! around as cheap copyable handles. The per-node input and output slot lists
//! are [`EntityList`]s of these ids, pooled inside the store.

use cranelift_entity::{EntityList, entity_impl};

/// A unique identifier for a node in the graph.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct NodeId(u32);
entity_impl!(NodeId, "node");

/// A unique identifier for one output slot of a node.
#[derive(Default, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ValueId(u32);
entity_impl!(ValueId, "%");

/// A unique identifier for one input slot of a node.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct UseId(u32);
entity_impl!(UseId, "use");

/// A list of input slot ids stored in an entity pool.
pub(crate) type UseIdList = EntityList<UseId>;

/// A list of output slot ids stored in an entity pool.
pub(crate) type ValueIdList = EntityList<ValueId>;

/// A vertex of the bipartite sea-of-nodes graph.
///
/// The graph is bipartite: nodes never connect directly to nodes, and values
/// never connect directly to values. A node's outputs are [`Vertex::Value`]s,
/// and a value's consumers are [`Vertex::Node`]s. The petgraph view
/// (the `petgraph_view` module) navigates over this enum so generic petgraph
/// algorithms (`toposort`, `DfsPostOrder`, …) can run on the graph.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Vertex {
    /// A node vertex.
    Node(NodeId),
    /// A value (node-output) vertex.
    Value(ValueId),
}
