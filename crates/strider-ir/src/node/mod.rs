//! Node identifiers, kinds, types, and per-arena entries for the IR graph.
//!
//! Public paths are preserved by `pub use`: `ir::node::NodeId`,
//! `ir::node::NodeKind`, etc. all resolve through this module.

mod data;
mod ids;
mod kind;
mod output_kind;
mod output_type;

#[cfg(test)]
mod tests;

pub(crate) use data::{Node, NodeInput, NodeOutput};
pub use ids::{NodeId, NodeInputId, NodeOutputId};
pub use kind::{FunctionArgSource, NodeCategory, NodeKind};
pub use output_kind::NodeOutputKind;
pub use output_type::NodeOutputType;

// Crate-private list-of-id aliases used by graph internals.
pub(crate) use ids::{NodeInputIdList, NodeOutputIdList};
