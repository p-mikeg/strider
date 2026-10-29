pub(crate) mod const_value;
mod kind;
mod ops;
mod value_kind;
mod value_type;
mod width_relation;

#[cfg(test)]
mod tests;

pub use crate::node_signature::{Conversion, ExpectedValueKind, OperandWidth};
pub use kind::{CcId, FunctionArgSource, InitialVnId, NodeKind, SwitchTableId};
pub use ops::{
    ExtendOp, FloatBinaryOp, FloatCmpOp, FloatUnaryOp, IntBinaryOp, IntCmpOp, IntUnaryOp,
};
pub use strider_graph::{NodeId, UseId, ValueId};
pub use value_kind::ValueKind;
pub use value_type::{ValueType, VnTypeExt};
pub use width_relation::{WidthGroups, WidthTie, width_tie};
