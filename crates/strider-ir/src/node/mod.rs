mod cast;
mod kind;
mod ops;
pub(crate) mod signature;
mod value_kind;
mod value_type;
mod width_relation;

#[cfg(test)]
mod tests;

pub use cast::{CastMask, cast_mask_of};
pub(crate) use kind::KindFlags;
pub use kind::{CcId, FunctionArgSource, InitialVnId, IntConstId, NodeKind, SwitchTableId};
pub use ops::{
    ExtendOp, FloatBinaryOp, FloatCmpOp, FloatUnaryOp, IntBinaryOp, IntCmpOp, IntUnaryOp,
};
pub use signature::{Conversion, ExpectedValueKind, OperandWidth};
pub use strider_graph::{NodeId, UseId, ValueId};
pub use value_kind::ValueKind;
pub use value_type::{ValueType, VnTypeExt};
pub use width_relation::{WidthTie, width_tie};
