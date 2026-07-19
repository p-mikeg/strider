pub(crate) mod const_value;
mod kind;
mod ops;
mod value_kind;
mod value_type;

#[cfg(test)]
mod tests;

pub use kind::{FunctionArgSource, InitialVnId, NodeKind};
pub use ops::{
    ExtendOp, FloatBinaryOp, FloatCmpOp, FloatUnaryOp, IntBinaryOp, IntCmpOp, IntUnaryOp,
};
pub use strider_graph::{NodeId, UseId, ValueId};
pub use value_kind::ValueKind;
pub use value_type::{ValueType, VnTypeExt};
