//! Operation kinds used by IR nodes.

mod op_kinds;

pub use op_kinds::{
    ExtendOp, FloatBinaryOp, FloatCmpOp, FloatUnaryOp, IntBinaryOp, IntCmpOp, IntUnaryOp,
};
