//! Generic typed binary-op builder shared by [`IntBinaryOp`] /
//! [`BoolBinaryOp`] / [`FloatBinaryOp`].
//!
//! The three families differ only in three small details:
//! * which `NodeKind` variant the op enum produces;
//! * the result type ([`BuildTy::InheritRoot`] for arithmetic / bitwise
//!   ops, [`BuildTy::Fixed(Bool)`] for boolean ops);
//! * which subset of variants is commutative.
//!
//! [`BinaryOpKind`] threads those three differences through one
//! generic [`BinaryOpPat<Op>`] type so the matcher / build-side code
//! stays factored.  Free constructors (`add`, `bool_and`, `float_add`,
//! …) re-export the three concrete instantiations as
//! [`IntBinaryOpPat`] / [`BoolBinaryOpPat`] / [`FloatBinaryOpPat`].

use ir::node::{NodeKind, NodeOutputType};
use ir::{BoolBinaryOp, FloatBinaryOp, IntBinaryOp};

use crate::matcher::commutativity::{
    is_commutative_bool_op, is_commutative_float_op, is_commutative_int_op,
};
use crate::pat::Pat;
use crate::pat::node_pat::{BuildTy, InputsSpec, KindSpec, NodePat};

/// Per-family parameters threaded through the generic [`BinaryOpPat`].
///
/// Implemented for every binary-op enum the IR exposes
/// ([`IntBinaryOp`] / [`BoolBinaryOp`] / [`FloatBinaryOp`]).  Crate-
/// private because `build_ty` returns the crate-private [`BuildTy`]
/// and the public surface only needs the three concrete alias types.
pub(crate) trait BinaryOpKind: Copy + 'static {
    fn node_kind(self) -> NodeKind;
    fn build_ty(self) -> BuildTy;
    fn is_commutative(self) -> bool;
}

impl BinaryOpKind for IntBinaryOp {
    fn node_kind(self) -> NodeKind { NodeKind::IntBinaryOp(self) }
    fn build_ty(self) -> BuildTy { BuildTy::InheritRoot }
    fn is_commutative(self) -> bool { is_commutative_int_op(self) }
}

impl BinaryOpKind for BoolBinaryOp {
    fn node_kind(self) -> NodeKind { NodeKind::BoolBinaryOp(self) }
    fn build_ty(self) -> BuildTy { BuildTy::Fixed(NodeOutputType::Bool) }
    fn is_commutative(self) -> bool { is_commutative_bool_op(self) }
}

impl BinaryOpKind for FloatBinaryOp {
    fn node_kind(self) -> NodeKind { NodeKind::FloatBinaryOp(self) }
    fn build_ty(self) -> BuildTy { BuildTy::InheritRoot }
    fn is_commutative(self) -> bool { is_commutative_float_op(self) }
}

/// Builder for a typed binary-op pattern over any `Op: BinaryOpKind`.
///
/// Constructed by free functions (`add`, `bool_and`, `float_add`, …)
/// or via the family-level dispatchers (`int_binary`, `bool_binary`,
/// `float_binary`).  See [`IntBinaryOpPat`] / [`BoolBinaryOpPat`] /
/// [`FloatBinaryOpPat`] for the three concrete instantiations the
/// public API exposes.
///
/// `BinaryOpKind` is crate-private — the only way to construct a
/// value of this type from outside the crate is through one of the
/// free constructors, which always returns one of the three aliased
/// instantiations.  `#[allow(private_bounds)]` documents that intent.
#[allow(private_bounds)]
pub struct BinaryOpPat<Op: BinaryOpKind> {
    op: Op,
    lhs: Pat,
    rhs: Pat,
    ordered: bool,
}

#[allow(private_bounds)]
impl<Op: BinaryOpKind> BinaryOpPat<Op> {
    pub(crate) fn new(op: Op, lhs: Pat, rhs: Pat) -> Self {
        Self { op, lhs, rhs, ordered: false }
    }

    /// Force the pattern to match operands in the stated order only.
    /// By default, commutative variants of the op family also try
    /// the reversed operand order.
    #[must_use]
    pub fn ordered(mut self) -> Self {
        self.ordered = true;
        self
    }
}

#[allow(private_bounds)]
impl<Op: BinaryOpKind> From<BinaryOpPat<Op>> for Pat {
    fn from(b: BinaryOpPat<Op>) -> Pat {
        let op = b.op;
        let inputs = if !b.ordered && op.is_commutative() {
            InputsSpec::fixed_commutative(b.lhs, b.rhs)
        } else {
            InputsSpec::fixed_ordered(vec![b.lhs, b.rhs])
        };
        let kind = op.node_kind();
        NodePat::matcher(KindSpec::Exact(kind), inputs)
            .with_build_exact(kind, op.build_ty())
            .into_pat()
    }
}

/// Back-compat alias: typed builder for integer binary-op patterns.
pub type IntBinaryOpPat = BinaryOpPat<IntBinaryOp>;
/// Back-compat alias: typed builder for boolean binary-op patterns.
pub type BoolBinaryOpPat = BinaryOpPat<BoolBinaryOp>;
/// Back-compat alias: typed builder for float binary-op patterns.
pub type FloatBinaryOpPat = BinaryOpPat<FloatBinaryOp>;
