//! Dense integer ids for nodes, outputs, and inputs.
//!
//! Each id is allocated from a [`cranelift_entity::PrimaryMap`] inside the
//! graph's [`crate::storage::RawStore`], so they are cheap copyable handles.
//! Per-node input and output slot lists are [`EntityList`]s of these ids,
//! pooled inside the store.

use cranelift_entity::{EntityList, entity_impl};

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct NodeId(u32);
entity_impl!(NodeId, "node");

/// One output slot of a node.
#[derive(Default, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ValueId(u32);
entity_impl!(ValueId, "%");

/// One input slot of a node.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct UseId(u32);
entity_impl!(UseId, "use");

pub(crate) type UseIdList = EntityList<UseId>;

pub(crate) type ValueIdList = EntityList<ValueId>;

/// A vertex of the bipartite graph: nodes never connect directly to nodes, and
/// values never directly to values. The petgraph view navigates over this enum
/// so generic petgraph algorithms can run on the graph.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Vertex {
    Node(NodeId),
    Value(ValueId),
}
