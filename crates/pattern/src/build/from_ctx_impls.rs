//! [`FromCtx`] implementations for every capture-variable type.
//!
//! Each impl is a thin wrapper over the corresponding
//! `Bindings::get_*` method, turning an absent binding into a
//! [`ErrorKind::MissingBinding`] error tagged with the capture-type name.

use ir::node::{NodeId, NodeOutputId};
use ir::{
    BoolBinaryOp, BoolUnaryOp, FloatBinaryOp, FloatCmpOp, FloatUnaryOp, IntBinaryOp, IntCmpOp,
    IntUnaryOp,
};

use crate::error::{ErrorKind, Result};
use crate::var::{
    BoolBinaryOpVar, BoolUnaryOpVar, BoolVar, FloatBinaryOpVar, FloatCmpOpVar, FloatUnaryOpVar,
    FloatVar, IntBinaryOpVar, IntCmpOpVar, IntUnaryOpVar, IntVar, NodeVar, Var,
};

use super::{BuildCtx, FromCtx};

/// Generate a [`FromCtx`] impl for a capture-variable type.
///
/// `$var` is the capture-variable type, `$out` is its `Output` associated
/// type (may be a tuple), `$getter` is the `Bindings` method used to look
/// up the binding, and `$tag` is the string tag for the
/// [`ErrorKind::MissingBinding`] error.
macro_rules! impl_from_ctx {
    ($var:ty, $out:ty, $getter:ident, $tag:literal) => {
        impl FromCtx for $var {
            type Output = $out;
            fn from_ctx(&self, ctx: &BuildCtx<'_>) -> Result<Self::Output> {
                ctx.bindings
                    .$getter(*self)
                    .ok_or_else(|| ErrorKind::MissingBinding($tag).into())
            }
        }
    };
}

impl_from_ctx!(Var, NodeOutputId, get, "Var");
impl_from_ctx!(NodeVar, NodeId, get_node, "NodeVar");
impl_from_ctx!(IntVar, u64, get_int, "IntVar");
impl_from_ctx!(BoolVar, bool, get_bool, "BoolVar");
impl_from_ctx!(FloatVar, u64, get_float_bits, "FloatVar");
impl_from_ctx!(IntBinaryOpVar, IntBinaryOp, get_int_binary_op, "IntBinaryOpVar");
impl_from_ctx!(IntUnaryOpVar, IntUnaryOp, get_int_unary_op, "IntUnaryOpVar");
impl_from_ctx!(IntCmpOpVar, IntCmpOp, get_int_cmp_op, "IntCmpOpVar");
impl_from_ctx!(BoolBinaryOpVar, BoolBinaryOp, get_bool_binary_op, "BoolBinaryOpVar");
impl_from_ctx!(BoolUnaryOpVar, BoolUnaryOp, get_bool_unary_op, "BoolUnaryOpVar");
impl_from_ctx!(FloatBinaryOpVar, FloatBinaryOp, get_float_binary_op, "FloatBinaryOpVar");
impl_from_ctx!(FloatUnaryOpVar, FloatUnaryOp, get_float_unary_op, "FloatUnaryOpVar");
impl_from_ctx!(FloatCmpOpVar, FloatCmpOp, get_float_cmp_op, "FloatCmpOpVar");
