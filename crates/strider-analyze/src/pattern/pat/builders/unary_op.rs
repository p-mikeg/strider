//! Shared backing helper for the per-family unary-op constructors
//! (`int_unary`, `bool_unary`, `float_unary`).
//!
//! Like [`super::binary_op`], these three functions only differ in
//! which `NodeKind::*UnaryOp` discriminant the op enum produces and
//! the result type — boolean unary ops are always `Bool`, integer /
//! float unary ops inherit the root type.  [`UnaryOpKind`] threads
//! both differences through one [`unary_pat`] helper so the family
//! ctors collapse to one-liners.

use strider_ir::node::{NodeKind, NodeOutputType};
use strider_ir::{BoolUnaryOp, FloatUnaryOp, IntUnaryOp};

use crate::pattern::pat::Pat;
use crate::pattern::pat::node_pat::{BuildTy, InputsSpec, KindSpec, NodePat};

/// Per-family parameters threaded through [`unary_pat`].  Crate-private
/// because `build_ty` returns the crate-private [`BuildTy`].
pub(crate) trait UnaryOpKind: Copy + 'static {
    fn node_kind(self) -> NodeKind;
    fn build_ty(self) -> BuildTy;
}

impl UnaryOpKind for IntUnaryOp {
    fn node_kind(self) -> NodeKind { NodeKind::IntUnaryOp(self) }
    fn build_ty(self) -> BuildTy { BuildTy::InheritRoot }
}

impl UnaryOpKind for BoolUnaryOp {
    fn node_kind(self) -> NodeKind { NodeKind::BoolUnaryOp(self) }
    fn build_ty(self) -> BuildTy { BuildTy::Fixed(NodeOutputType::Bool) }
}

impl UnaryOpKind for FloatUnaryOp {
    fn node_kind(self) -> NodeKind { NodeKind::FloatUnaryOp(self) }
    fn build_ty(self) -> BuildTy { BuildTy::InheritRoot }
}

/// Build a unary-op pattern parameterised by the op family.  Used by
/// the per-family `int_unary` / `bool_unary` / `float_unary` ctors;
/// not re-exported because the trait is crate-private.
pub(crate) fn unary_pat<Op: UnaryOpKind>(op: Op, operand: Pat) -> Pat {
    let kind = op.node_kind();
    NodePat::matcher(KindSpec::Exact(kind), InputsSpec::fixed_ordered(vec![operand]))
        .with_build_exact(kind, op.build_ty())
        .into_pat()
}
