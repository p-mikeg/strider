//! Node identifiers, kinds, types, and per-arena entries for the IR graph.
//!
//! Public paths are preserved by `pub use`: `ir::node::NodeId`,
//! `ir::node::NodeKind`, etc. all resolve through this module.

pub(crate) mod const_value;
mod kind;
mod ops;
mod value_kind;
mod value_type;

#[cfg(test)]
mod tests;

// The structural ids are the generic graph's ids — re-exported here so every
// downstream `use strider_ir::node::{NodeId, ValueId, UseId}` keeps resolving.
pub use kind::{FunctionArgSource, InitialVnId, NodeKind};
pub use ops::{
    ExtendOp, FloatBinaryOp, FloatCmpOp, FloatUnaryOp, IntBinaryOp, IntCmpOp, IntUnaryOp,
};
pub use strider_graph::{NodeId, UseId, ValueId};
pub use value_kind::ValueKind;
pub use value_type::{ValueType, VnTypeExt};
