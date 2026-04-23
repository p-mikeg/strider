//! Variant-agnostic ("`*_any`") op constructors.
//!
//! These patterns match **any** variant of an op family (int binary, bool
//! unary, …) and bind the actual operator variant to a typed capture variable.

use std::sync::Arc;

use ir::node::NodeKind;

use crate::matcher::commutativity::{is_commutative_int_cmp_op, is_commutative_int_op};
use crate::pat::node_pat::{InputsSpec, NodePat};
use crate::pat::{Pat, PatKind};
use crate::var::{
    BoolBinaryOpVar, BoolUnaryOpVar, FloatBinaryOpVar, FloatCmpOpVar, FloatUnaryOpVar,
    IntBinaryOpVar, IntCmpOpVar, IntUnaryOpVar,
};

/// Matches **any** integer binary operation and binds the actual operator
/// variant to `op`.
///
/// Commutative ops (`Add`, `Mul`, `And`, `Or`, `Xor`) will try both operand
/// orderings automatically.  Because `int_binary_any` returns a `Pat` directly
/// rather than a builder, there is no `.ordered()` method.
pub fn int_binary_any(op_var: IntBinaryOpVar, lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> Pat {
    // At construction time: match any IntBinaryOp node; at match time decide
    // commutativity based on the concrete op variant observed on the node.
    let inputs = InputsSpec::fixed_maybe_commutative(lhs.into(), rhs.into(), |ctx, node| {
        match ctx.graph.graph.node_kind(node) {
            NodeKind::IntBinaryOp(op) => is_commutative_int_op(*op),
            _ => false,
        }
    });
    Pat::from_dyn(Arc::new(NodePat {
        kind_match: Arc::new(|ctx, node, _b| {
            matches!(ctx.graph.graph.node_kind(node), NodeKind::IntBinaryOp(_))
        }),
        inputs,
        post_match: Some(Arc::new(move |ctx, node, b| {
            match ctx.graph.graph.node_kind(node) {
                NodeKind::IntBinaryOp(op) => b.bind_int_binary_op(op_var, *op),
                _ => false,
            }
        })),
        output_var: None,
        node_var: None,
    }))
}

/// Matches **any** integer unary operation and binds the actual operator
/// variant to `op`.
pub fn int_unary_any(op_var: IntUnaryOpVar, operand: impl Into<Pat>) -> Pat {
    Pat::from_dyn(Arc::new(NodePat {
        kind_match: Arc::new(|ctx, node, _b| {
            matches!(ctx.graph.graph.node_kind(node), NodeKind::IntUnaryOp(_))
        }),
        inputs: InputsSpec::fixed_ordered(vec![operand.into()]),
        post_match: Some(Arc::new(move |ctx, node, b| {
            match ctx.graph.graph.node_kind(node) {
                NodeKind::IntUnaryOp(op) => b.bind_int_unary_op(op_var, *op),
                _ => false,
            }
        })),
        output_var: None,
        node_var: None,
    }))
}

/// Matches **any** integer comparison and binds the actual operator variant
/// to `op`.
///
/// Commutative comparisons (`Equal`, `Carry`, `Scarry`) try both operand
/// orderings automatically.
pub fn int_cmp_any(op_var: IntCmpOpVar, lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> Pat {
    let inputs = InputsSpec::fixed_maybe_commutative(lhs.into(), rhs.into(), |ctx, node| {
        match ctx.graph.graph.node_kind(node) {
            NodeKind::IntCmpOp(op) => is_commutative_int_cmp_op(*op),
            _ => false,
        }
    });
    Pat::from_dyn(Arc::new(NodePat {
        kind_match: Arc::new(|ctx, node, _b| {
            matches!(ctx.graph.graph.node_kind(node), NodeKind::IntCmpOp(_))
        }),
        inputs,
        post_match: Some(Arc::new(move |ctx, node, b| {
            match ctx.graph.graph.node_kind(node) {
                NodeKind::IntCmpOp(op) => b.bind_int_cmp_op(op_var, *op),
                _ => false,
            }
        })),
        output_var: None,
        node_var: None,
    }))
}

/// Matches **any** boolean binary operation and binds the actual operator
/// variant to `op`.
///
/// Commutative ops (`And`, `Or`, `Xor`) try both operand orderings
/// automatically.
pub fn bool_binary_any(op: BoolBinaryOpVar, lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> Pat {
    Pat::new(PatKind::BoolBinaryAny {
        op,
        lhs: lhs.into(),
        rhs: rhs.into(),
        ordered: false,
    })
}

/// Matches **any** boolean unary operation and binds the actual operator
/// variant to `op`.
pub fn bool_unary_any(op: BoolUnaryOpVar, operand: impl Into<Pat>) -> Pat {
    Pat::new(PatKind::BoolUnaryAny {
        op,
        operand: operand.into(),
    })
}

/// Matches **any** float binary operation and binds the actual operator
/// variant to `op`.
///
/// Commutative ops (`Add`, `Mul`) try both operand orderings automatically.
pub fn float_binary_any(op: FloatBinaryOpVar, lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> Pat {
    Pat::new(PatKind::FloatBinaryAny {
        op,
        lhs: lhs.into(),
        rhs: rhs.into(),
        ordered: false,
    })
}

/// Matches **any** float unary operation and binds the actual operator
/// variant to `op`.
pub fn float_unary_any(op: FloatUnaryOpVar, operand: impl Into<Pat>) -> Pat {
    Pat::new(PatKind::FloatUnaryAny {
        op,
        operand: operand.into(),
    })
}

/// Matches **any** float comparison and binds the actual operator variant
/// to `op`.
///
/// No float comparison operators are currently treated as commutative, so no
/// automatic operand-swap retry is attempted.
pub fn float_cmp_any(op: FloatCmpOpVar, lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> Pat {
    Pat::new(PatKind::FloatCmpAny {
        op,
        lhs: lhs.into(),
        rhs: rhs.into(),
        ordered: false,
    })
}
