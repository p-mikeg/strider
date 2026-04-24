//! Typed binary-op builders (`IntBinaryOpPat`, `BoolBinaryOpPat`,
//! `FloatBinaryOpPat`).  Each carries an `op` + two sub-patterns and a
//! default-commutative retry governed by `is_commutative_*`.

use ir::node::{NodeKind, NodeOutputType};
use ir::{BoolBinaryOp, FloatBinaryOp, IntBinaryOp};

use crate::matcher::commutativity::{
    is_commutative_bool_op, is_commutative_float_op, is_commutative_int_op,
};
use crate::pat::Pat;
use crate::pat::node_pat::{BuildTy, InputsSpec, KindSpec, NodePat};

/// Helper shared by the three typed binary-op builders.
fn binary_op_pat(
    kind: NodeKind,
    build_ty: BuildTy,
    inputs: InputsSpec,
) -> Pat {
    NodePat::matcher(KindSpec::Exact(kind), inputs)
        .with_build_exact(kind, build_ty)
        .into_pat()
}

// ── IntBinaryOpPat ────────────────────────────────────────────────────────────

/// Builder for integer binary operation patterns.
pub struct IntBinaryOpPat {
    pub(super) op: IntBinaryOp,
    pub(super) lhs: Pat,
    pub(super) rhs: Pat,
    pub(super) ordered: bool,
}

impl IntBinaryOpPat {
    pub(crate) fn new(op: IntBinaryOp, lhs: Pat, rhs: Pat) -> Self {
        Self { op, lhs, rhs, ordered: false }
    }

    /// Force the pattern to match operands in the stated order only.
    /// By default, commutative operators (`Add`, `Mul`, `And`, `Or`, `Xor`)
    /// will also try the reversed operand order.
    #[must_use]
    pub fn ordered(mut self) -> Self {
        self.ordered = true;
        self
    }
}

impl From<IntBinaryOpPat> for Pat {
    fn from(b: IntBinaryOpPat) -> Pat {
        let op = b.op;
        let inputs = if !b.ordered && is_commutative_int_op(op) {
            InputsSpec::fixed_commutative(b.lhs, b.rhs)
        } else {
            InputsSpec::fixed_ordered(vec![b.lhs, b.rhs])
        };
        binary_op_pat(NodeKind::IntBinaryOp(op), BuildTy::InheritRoot, inputs)
    }
}

// ── BoolBinaryOpPat ───────────────────────────────────────────────────────────

/// Builder for boolean binary operation patterns.
pub struct BoolBinaryOpPat {
    pub(super) op: BoolBinaryOp,
    pub(super) lhs: Pat,
    pub(super) rhs: Pat,
    pub(super) ordered: bool,
}

impl BoolBinaryOpPat {
    pub(crate) fn new(op: BoolBinaryOp, lhs: Pat, rhs: Pat) -> Self {
        Self { op, lhs, rhs, ordered: false }
    }

    /// Force the pattern to match operands in the stated order only.
    #[must_use]
    pub fn ordered(mut self) -> Self {
        self.ordered = true;
        self
    }
}

impl From<BoolBinaryOpPat> for Pat {
    fn from(b: BoolBinaryOpPat) -> Pat {
        let op = b.op;
        let inputs = if !b.ordered && is_commutative_bool_op(op) {
            InputsSpec::fixed_commutative(b.lhs, b.rhs)
        } else {
            InputsSpec::fixed_ordered(vec![b.lhs, b.rhs])
        };
        binary_op_pat(NodeKind::BoolBinaryOp(op), BuildTy::Fixed(NodeOutputType::Bool), inputs)
    }
}

// ── FloatBinaryOpPat ──────────────────────────────────────────────────────────

/// Builder for float binary operation patterns.
pub struct FloatBinaryOpPat {
    pub(super) op: FloatBinaryOp,
    pub(super) lhs: Pat,
    pub(super) rhs: Pat,
    pub(super) ordered: bool,
}

impl FloatBinaryOpPat {
    pub(crate) fn new(op: FloatBinaryOp, lhs: Pat, rhs: Pat) -> Self {
        Self { op, lhs, rhs, ordered: false }
    }

    /// Force the pattern to match operands in the stated order only.
    /// By default, commutative operators (`Add`, `Mul`) will also try the
    /// reversed operand order.
    #[must_use]
    pub fn ordered(mut self) -> Self {
        self.ordered = true;
        self
    }
}

impl From<FloatBinaryOpPat> for Pat {
    fn from(b: FloatBinaryOpPat) -> Pat {
        let op = b.op;
        let inputs = if !b.ordered && is_commutative_float_op(op) {
            InputsSpec::fixed_commutative(b.lhs, b.rhs)
        } else {
            InputsSpec::fixed_ordered(vec![b.lhs, b.rhs])
        };
        binary_op_pat(NodeKind::FloatBinaryOp(op), BuildTy::InheritRoot, inputs)
    }
}
