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
pub use value_type::{ValueType, VnTypeExt, low_bits_mask_u128};
pub(crate) use value_type::{complement_limbs, shifted_ones_limbs};
pub use width_relation::{WidthGroups, WidthTie, width_tie};
