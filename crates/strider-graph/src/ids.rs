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
/// values never directly to values.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Vertex {
    Node(NodeId),
    Value(ValueId),
}
