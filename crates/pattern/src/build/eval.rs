//! Recursive evaluator for a [`super::Build`] tree.
//!
//! Converts an RHS [`super::Build`] value into a concrete set of fresh IR
//! nodes spliced into a [`BuiltFunctionGraph`], propagating [`InnerOutcome::Skip`]
//! up through the tree so any sub-closure can abort the whole rewrite.

use std::sync::Arc;

use ir::BuiltFunctionGraph;
use ir::node::{NodeId, NodeKind, NodeOutputType};

use crate::error::{ErrorKind, Result};
use crate::matcher::Bindings;

use super::Build;
use super::ctx::BuildCtx;
use super::rule::{InnerOutcome, RewriteOutcome};

/// Scratch state threaded through the recursive evaluator.  Mutable reference
/// to the graph, plus the immutable match context needed by every closure.
pub(super) struct EvalState<'a> {
    pub(super) fg: &'a mut BuiltFunctionGraph,
    pub(super) bindings: &'a Bindings,
    pub(super) root: NodeId,
    pub(super) root_ty: NodeOutputType,
}

impl<'a> EvalState<'a> {
    fn build_ctx(&self) -> BuildCtx<'_> {
        BuildCtx {
            graph: self.fg,
            bindings: self.bindings,
            root: self.root,
            root_ty: self.root_ty,
        }
    }
}

pub(super) fn eval_subtree(
    state: &mut EvalState<'_>,
    build: &Build,
) -> Result<InnerOutcome> {
    match build {
        Build::Skip => Ok(InnerOutcome::Skip),

        Build::Capture(v) => {
            let out = state.bindings.get(*v).ok_or_else(|| {
                ErrorKind::AssertionFailed(format!(
                    "Build::Capture references unbound Var {v:?}"
                ))
            })?;
            Ok(InnerOutcome::Out(out))
        }

        Build::IntConst(bv) => {
            let val = bv.resolve(&state.build_ctx())?;
            let out = state.fg.make_int_const(val, state.root_ty)?;
            Ok(InnerOutcome::Out(out))
        }

        Build::BoolConst(bv) => {
            let val = bv.resolve(&state.build_ctx())?;
            let out = state.fg.make_bool_const(val)?;
            Ok(InnerOutcome::Out(out))
        }

        Build::FloatConst(bv) => {
            let bits = bv.resolve(&state.build_ctx())?;
            let out = state.fg.make_float_const(bits, state.root_ty)?;
            Ok(InnerOutcome::Out(out))
        }

        Build::IntBinary(op, l, r) => {
            build_binary(state, l, r, NodeKind::IntBinaryOp(*op), state.root_ty)
        }
        Build::IntUnary(op, x) => {
            build_unary(state, x, NodeKind::IntUnaryOp(*op), state.root_ty)
        }
        Build::IntCmp(op, l, r) => build_binary(
            state,
            l,
            r,
            NodeKind::IntCmpOp(*op),
            NodeOutputType::Bool,
        ),

        Build::BoolBinary(op, l, r) => build_binary(
            state,
            l,
            r,
            NodeKind::BoolBinaryOp(*op),
            NodeOutputType::Bool,
        ),
        Build::BoolUnary(op, x) => build_unary(
            state,
            x,
            NodeKind::BoolUnaryOp(*op),
            NodeOutputType::Bool,
        ),

        Build::FloatBinary(op, l, r) => build_binary(
            state,
            l,
            r,
            NodeKind::FloatBinaryOp(*op),
            state.root_ty,
        ),
        Build::FloatUnary(op, x) => {
            build_unary(state, x, NodeKind::FloatUnaryOp(*op), state.root_ty)
        }
        Build::FloatCmp(op, l, r) => build_binary(
            state,
            l,
            r,
            NodeKind::FloatCmpOp(*op),
            NodeOutputType::Bool,
        ),

        Build::IntBinaryFromVar(v, l, r) => {
            let op = state.bindings.get_int_binary_op(*v).ok_or_else(|| {
                ErrorKind::AssertionFailed(format!(
                    "Build::IntBinaryFromVar references unbound IntBinaryOpVar {v:?}"
                ))
            })?;
            build_binary(state, l, r, NodeKind::IntBinaryOp(op), state.root_ty)
        }
        Build::IntUnaryFromVar(v, x) => {
            let op = state.bindings.get_int_unary_op(*v).ok_or_else(|| {
                ErrorKind::AssertionFailed(format!(
                    "Build::IntUnaryFromVar references unbound IntUnaryOpVar {v:?}"
                ))
            })?;
            build_unary(state, x, NodeKind::IntUnaryOp(op), state.root_ty)
        }
        Build::IntCmpFromVar(v, l, r) => {
            let op = state.bindings.get_int_cmp_op(*v).ok_or_else(|| {
                ErrorKind::AssertionFailed(format!(
                    "Build::IntCmpFromVar references unbound IntCmpOpVar {v:?}"
                ))
            })?;
            build_binary(
                state,
                l,
                r,
                NodeKind::IntCmpOp(op),
                NodeOutputType::Bool,
            )
        }
        Build::BoolBinaryFromVar(v, l, r) => {
            let op = state.bindings.get_bool_binary_op(*v).ok_or_else(|| {
                ErrorKind::AssertionFailed(format!(
                    "Build::BoolBinaryFromVar references unbound BoolBinaryOpVar {v:?}"
                ))
            })?;
            build_binary(
                state,
                l,
                r,
                NodeKind::BoolBinaryOp(op),
                NodeOutputType::Bool,
            )
        }
        Build::BoolUnaryFromVar(v, x) => {
            let op = state.bindings.get_bool_unary_op(*v).ok_or_else(|| {
                ErrorKind::AssertionFailed(format!(
                    "Build::BoolUnaryFromVar references unbound BoolUnaryOpVar {v:?}"
                ))
            })?;
            build_unary(state, x, NodeKind::BoolUnaryOp(op), NodeOutputType::Bool)
        }
        Build::FloatBinaryFromVar(v, l, r) => {
            let op = state.bindings.get_float_binary_op(*v).ok_or_else(|| {
                ErrorKind::AssertionFailed(format!(
                    "Build::FloatBinaryFromVar references unbound FloatBinaryOpVar {v:?}"
                ))
            })?;
            build_binary(state, l, r, NodeKind::FloatBinaryOp(op), state.root_ty)
        }
        Build::FloatUnaryFromVar(v, x) => {
            let op = state.bindings.get_float_unary_op(*v).ok_or_else(|| {
                ErrorKind::AssertionFailed(format!(
                    "Build::FloatUnaryFromVar references unbound FloatUnaryOpVar {v:?}"
                ))
            })?;
            build_unary(state, x, NodeKind::FloatUnaryOp(op), state.root_ty)
        }
        Build::FloatCmpFromVar(v, l, r) => {
            let op = state.bindings.get_float_cmp_op(*v).ok_or_else(|| {
                ErrorKind::AssertionFailed(format!(
                    "Build::FloatCmpFromVar references unbound FloatCmpOpVar {v:?}"
                ))
            })?;
            build_binary(
                state,
                l,
                r,
                NodeKind::FloatCmpOp(op),
                NodeOutputType::Bool,
            )
        }
    }
}

fn build_unary(
    state: &mut EvalState<'_>,
    x: &Arc<Build>,
    kind: NodeKind,
    result_ty: NodeOutputType,
) -> Result<InnerOutcome> {
    let InnerOutcome::Out(arg) = eval_subtree(state, x)? else {
        return Ok(InnerOutcome::Skip);
    };
    let out = state.fg.make_value_node(kind, [arg], result_ty)?;
    Ok(InnerOutcome::Out(out))
}

fn build_binary(
    state: &mut EvalState<'_>,
    l: &Arc<Build>,
    r: &Arc<Build>,
    kind: NodeKind,
    result_ty: NodeOutputType,
) -> Result<InnerOutcome> {
    let InnerOutcome::Out(l_out) = eval_subtree(state, l)? else {
        return Ok(InnerOutcome::Skip);
    };
    let InnerOutcome::Out(r_out) = eval_subtree(state, r)? else {
        return Ok(InnerOutcome::Skip);
    };
    let out = state.fg.make_value_node(kind, [l_out, r_out], result_ty)?;
    Ok(InnerOutcome::Out(out))
}

/// Top-level evaluator.  Converts [`InnerOutcome::Skip`] into
/// [`RewriteOutcome::Skip`] and wraps a produced output in
/// [`RewriteOutcome::RedirectTo`].
pub fn eval(
    build: &Build,
    fg: &mut BuiltFunctionGraph,
    bindings: &Bindings,
    root: NodeId,
    root_ty: NodeOutputType,
) -> Result<RewriteOutcome> {
    let mut state = EvalState {
        fg,
        bindings,
        root,
        root_ty,
    };
    match eval_subtree(&mut state, build)? {
        InnerOutcome::Out(out) => Ok(RewriteOutcome::RedirectTo(out)),
        InnerOutcome::Skip => Ok(RewriteOutcome::Skip),
    }
}
