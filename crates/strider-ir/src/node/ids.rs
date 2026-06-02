//! Dense integer identifiers for nodes, outputs, and inputs.
//!
//! Each id is a small integer allocated from a [`cranelift_entity::PrimaryMap`]
//! on `Graph`, so they can be passed around as cheap copyable handles. The
//! per-node input and output slot lists are [`EntityList`]s of these ids,
//! pooled inside the graph.

use cranelift_entity::{EntityList, entity_impl};

/// A unique identifier for a node in the IR graph.
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
entity_impl!(UseId, "input");

/// A list of input slot ids stored in an entity pool.
pub(crate) type NodeInputIdList = EntityList<UseId>;

/// A list of output slot ids stored in an entity pool.
pub(crate) type ValueIdList = EntityList<ValueId>;
