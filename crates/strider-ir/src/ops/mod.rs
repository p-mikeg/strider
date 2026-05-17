//! Operation kinds used by IR nodes, plus helper methods on
//! [`crate::function::BuiltFunctionGraph`] grouped by purpose:
//!
//! - [`consts`] — constant-output inspection & creation.
//! - [`rewrite`] — graph-mutation helpers like `replace_all_uses`.
//! - [`builder`] — ergonomic node constructors.

mod op_kinds;

pub use op_kinds::{
    BoolBinaryOp, BoolUnaryOp, ExtendOp, FloatBinaryOp, FloatCmpOp, FloatUnaryOp, IntBinaryOp,
    IntCmpOp, IntUnaryOp,
};

pub mod builder;
pub mod consts;
pub mod rewrite;
