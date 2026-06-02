//! `Node`, `NodeInput`, and `NodeOutput` — the per-arena entries that hold
//! per-node metadata, the use-list backbone, and the stable `(producer,
//! output_index)` mapping.

use cranelift_entity::packed_option::PackedOption;

use super::ids::{NodeId, UseId, NodeInputIdList, ValueId, ValueIdList};
use super::kind::NodeKind;
use super::value_kind::ValueKind;

/// Stores the output of a given node and tracks all of its uses via a
/// linked list of [`NodeInput`] ids.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NodeOutput {
    /// What kind of value this output carries.
    pub(crate) kind: ValueKind,
    /// The node that produces this output.
    pub(crate) source_id: NodeId,
    /// The index of this output in the source node's output list.
    pub(crate) output_index: u32,
    /// Head of the linked list of all inputs that consume this output.
    pub(crate) first_use: PackedOption<UseId>,
}

impl NodeOutput {
    /// Creates a new `NodeOutput` with no uses yet.
    #[must_use]
    pub(crate) fn new(kind: ValueKind, source_id: NodeId, output_index: u32) -> Self {
        NodeOutput {
            kind,
            source_id,
            output_index,
            first_use: None.into(),
        }
    }
}

/// Records a single use of a [`NodeOutput`] as the input of some node.
///
/// Forms part of a doubly-linked list of all uses of a particular output,
/// enabling efficient update of all consumers when an output changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NodeInput {
    /// The output being consumed.
    pub(crate) output_id: ValueId,
    /// Previous use in the linked list of uses for `output_id`.
    pub(crate) prev: PackedOption<UseId>,
    /// Next use in the linked list of uses for `output_id`.
    pub(crate) next: PackedOption<UseId>,
    /// The node that consumes this input.
    pub(crate) node_id: NodeId,
    /// The position of this input in the consuming node's input list.
    pub(crate) input_index: u32,
}

impl NodeInput {
    /// Creates a new `NodeInput` not yet linked into any use list.
    #[must_use]
    pub(crate) fn new(output_id: ValueId, node_id: NodeId, input_index: u32) -> Self {
        NodeInput {
            output_id,
            prev: None.into(),
            next: None.into(),
            node_id,
            input_index,
        }
    }
}

/// A node in the IR graph.
///
/// Holds the node's kind along with its input and output slot lists (stored
/// externally in entity pools).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Node {
    pub(crate) kind: NodeKind,
    pub(crate) inputs: NodeInputIdList,
    pub(crate) outputs: ValueIdList,
}

impl Node {
    /// Creates a new node with the given kind and empty input/output lists.
    #[must_use]
    pub(crate) fn new(kind: NodeKind) -> Self {
        Self {
            kind,
            inputs: NodeInputIdList::new(),
            outputs: ValueIdList::new(),
        }
    }
}
