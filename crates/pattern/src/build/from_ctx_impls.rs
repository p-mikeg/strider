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

impl FromCtx for Var {
    type Output = NodeOutputId;
    fn from_ctx(&self, ctx: &BuildCtx<'_>) -> Result<Self::Output> {
        ctx.bindings
            .get(*self)
            .ok_or_else(|| ErrorKind::MissingBinding("Var").into())
    }
}

impl FromCtx for NodeVar {
    type Output = NodeId;
    fn from_ctx(&self, ctx: &BuildCtx<'_>) -> Result<Self::Output> {
        ctx.bindings
            .get_node(*self)
            .ok_or_else(|| ErrorKind::MissingBinding("NodeVar").into())
    }
}

impl FromCtx for IntVar {
    type Output = u64;
    fn from_ctx(&self, ctx: &BuildCtx<'_>) -> Result<Self::Output> {
        ctx.bindings
            .get_int(*self)
            .ok_or_else(|| ErrorKind::MissingBinding("IntVar").into())
    }
}

impl FromCtx for BoolVar {
    type Output = bool;
    fn from_ctx(&self, ctx: &BuildCtx<'_>) -> Result<Self::Output> {
        ctx.bindings
            .get_bool(*self)
            .ok_or_else(|| ErrorKind::MissingBinding("BoolVar").into())
    }
}

impl FromCtx for FloatVar {
    type Output = u64;
    fn from_ctx(&self, ctx: &BuildCtx<'_>) -> Result<Self::Output> {
        ctx.bindings
            .get_float_bits(*self)
            .ok_or_else(|| ErrorKind::MissingBinding("FloatVar").into())
    }
}

impl FromCtx for IntBinaryOpVar {
    type Output = IntBinaryOp;
    fn from_ctx(&self, ctx: &BuildCtx<'_>) -> Result<Self::Output> {
        ctx.bindings
            .get_int_binary_op(*self)
            .ok_or_else(|| ErrorKind::MissingBinding("IntBinaryOpVar").into())
    }
}

impl FromCtx for IntUnaryOpVar {
    type Output = IntUnaryOp;
    fn from_ctx(&self, ctx: &BuildCtx<'_>) -> Result<Self::Output> {
        ctx.bindings
            .get_int_unary_op(*self)
            .ok_or_else(|| ErrorKind::MissingBinding("IntUnaryOpVar").into())
    }
}

impl FromCtx for IntCmpOpVar {
    type Output = IntCmpOp;
    fn from_ctx(&self, ctx: &BuildCtx<'_>) -> Result<Self::Output> {
        ctx.bindings
            .get_int_cmp_op(*self)
            .ok_or_else(|| ErrorKind::MissingBinding("IntCmpOpVar").into())
    }
}

impl FromCtx for BoolBinaryOpVar {
    type Output = BoolBinaryOp;
    fn from_ctx(&self, ctx: &BuildCtx<'_>) -> Result<Self::Output> {
        ctx.bindings
            .get_bool_binary_op(*self)
            .ok_or_else(|| ErrorKind::MissingBinding("BoolBinaryOpVar").into())
    }
}

impl FromCtx for BoolUnaryOpVar {
    type Output = BoolUnaryOp;
    fn from_ctx(&self, ctx: &BuildCtx<'_>) -> Result<Self::Output> {
        ctx.bindings
            .get_bool_unary_op(*self)
            .ok_or_else(|| ErrorKind::MissingBinding("BoolUnaryOpVar").into())
    }
}

impl FromCtx for FloatBinaryOpVar {
    type Output = FloatBinaryOp;
    fn from_ctx(&self, ctx: &BuildCtx<'_>) -> Result<Self::Output> {
        ctx.bindings
            .get_float_binary_op(*self)
            .ok_or_else(|| ErrorKind::MissingBinding("FloatBinaryOpVar").into())
    }
}

impl FromCtx for FloatUnaryOpVar {
    type Output = FloatUnaryOp;
    fn from_ctx(&self, ctx: &BuildCtx<'_>) -> Result<Self::Output> {
        ctx.bindings
            .get_float_unary_op(*self)
            .ok_or_else(|| ErrorKind::MissingBinding("FloatUnaryOpVar").into())
    }
}

impl FromCtx for FloatCmpOpVar {
    type Output = FloatCmpOp;
    fn from_ctx(&self, ctx: &BuildCtx<'_>) -> Result<Self::Output> {
        ctx.bindings
            .get_float_cmp_op(*self)
            .ok_or_else(|| ErrorKind::MissingBinding("FloatCmpOpVar").into())
    }
}
