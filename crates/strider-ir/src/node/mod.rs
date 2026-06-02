//! Node identifiers, kinds, types, and per-arena entries for the IR graph.
//!
//! Public paths are preserved by `pub use`: `ir::node::NodeId`,
//! `ir::node::NodeKind`, etc. all resolve through this module.

mod data;
mod ids;
mod kind;
mod value_kind;
mod value_type;

#[cfg(test)]
mod tests;

pub(crate) use data::{Node, NodeInput, NodeOutput};
pub use ids::{NodeId, UseId, ValueId};
pub use kind::{FunctionArgSource, NodeKind};
pub use value_kind::ValueKind;
pub use value_type::ValueType;

// Crate-private list-of-id aliases used by graph internals.
pub(crate) use ids::{NodeInputIdList, ValueIdList};
