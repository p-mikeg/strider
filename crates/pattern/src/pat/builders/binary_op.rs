//! Typed binary-op builders (`IntBinaryOpPat`, `BoolBinaryOpPat`,
//! `FloatBinaryOpPat`).  Each carries an `op` + two sub-patterns and a
//! default-commutative retry governed by `is_commutative_*`.

use std::sync::Arc;

use ir::node::{NodeKind, NodeOutputType};
use ir::{BoolBinaryOp, FloatBinaryOp, IntBinaryOp};

use crate::matcher::commutativity::{
    is_commutative_bool_op, is_commutative_float_op, is_commutative_int_op,
};
use crate::pat::Pat;
use crate::pat::node_pat::{
    BuildTy, InputsSpec, KindFilter, NodeKindBuilder, NodeKindCheck, NodePat,
};

/// Helper shared by the three typed binary-op builders.
fn binary_op_pat(
    root_kind: KindFilter,
    kind_match: NodeKindCheck,
    kind_build: NodeKindBuilder,
    build_ty: BuildTy,
    inputs: InputsSpec,
) -> Pat {
    NodePat::matcher(root_kind, kind_match, inputs)
        .with_build(kind_build)
        .with_build_ty(build_ty)
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
        binary_op_pat(
            KindFilter::exact(&NodeKind::IntBinaryOp(op)),
            Arc::new(move |ctx, node, _b| {
                matches!(ctx.graph.graph.node_kind(node), NodeKind::IntBinaryOp(x) if *x == op)
            }),
            Arc::new(move |_b| Ok(NodeKind::IntBinaryOp(op))),
            BuildTy::InheritRoot,
            inputs,
        )
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
        binary_op_pat(
            KindFilter::exact(&NodeKind::BoolBinaryOp(op)),
            Arc::new(move |ctx, node, _b| {
                matches!(ctx.graph.graph.node_kind(node), NodeKind::BoolBinaryOp(x) if *x == op)
            }),
            Arc::new(move |_b| Ok(NodeKind::BoolBinaryOp(op))),
            BuildTy::Fixed(NodeOutputType::Bool),
            inputs,
        )
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
        binary_op_pat(
            KindFilter::exact(&NodeKind::FloatBinaryOp(op)),
            Arc::new(move |ctx, node, _b| {
                matches!(ctx.graph.graph.node_kind(node), NodeKind::FloatBinaryOp(x) if *x == op)
            }),
            Arc::new(move |_b| Ok(NodeKind::FloatBinaryOp(op))),
            BuildTy::InheritRoot,
            inputs,
        )
    }
}
