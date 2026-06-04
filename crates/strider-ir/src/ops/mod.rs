//! Operation kinds used by IR nodes, plus helper methods on
//! [`crate::graph::Graph`] grouped by purpose:
//!
//! - [`consts`] — constant-output inspection.
//! - [`rewrite`] — graph-mutation helpers like `replace_all_uses`.

mod op_kinds;

pub use op_kinds::{
    ExtendOp, FloatBinaryOp, FloatCmpOp, FloatUnaryOp, IntBinaryOp, IntCmpOp, IntUnaryOp,
};

pub mod consts;
pub mod rewrite;
