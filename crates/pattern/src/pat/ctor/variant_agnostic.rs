//! Variant-agnostic ("`*_any`") op constructors.
//!
//! These patterns match **any** variant of an op family (int binary, bool
//! unary, …) and bind the actual operator variant to a typed capture variable.

use std::sync::Arc;

use ir::node::NodeKind;

use crate::matcher::commutativity::{
    is_commutative_bool_op, is_commutative_float_op, is_commutative_int_cmp_op,
    is_commutative_int_op,
};
use crate::pat::Pat;
use crate::pat::node_pat::{InputsSpec, NodePat};
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
        kind_build: Some(Arc::new(move |ctx| {
            let op = ctx.bindings
                .get_int_binary_op(op_var)
                .ok_or(crate::error::ErrorKind::MissingBinding("IntBinaryOpVar"))?;
            Ok(NodeKind::IntBinaryOp(op))
        })),
        build_result_ty: crate::pat::node_pat::BuildTy::InheritRoot,
        outputs: crate::pat::node_pat::OutputsSpec::None,
        consumers: crate::pat::node_pat::ConsumersSpec::None,
        candidate_kind: None,
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
        kind_build: Some(Arc::new(move |ctx| {
            let op = ctx.bindings
                .get_int_unary_op(op_var)
                .ok_or(crate::error::ErrorKind::MissingBinding("IntUnaryOpVar"))?;
            Ok(NodeKind::IntUnaryOp(op))
        })),
        build_result_ty: crate::pat::node_pat::BuildTy::InheritRoot,
        outputs: crate::pat::node_pat::OutputsSpec::None,
        consumers: crate::pat::node_pat::ConsumersSpec::None,
        candidate_kind: None,
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
        kind_build: Some(Arc::new(move |ctx| {
            let op = ctx.bindings
                .get_int_cmp_op(op_var)
                .ok_or(crate::error::ErrorKind::MissingBinding("IntCmpOpVar"))?;
            Ok(NodeKind::IntCmpOp(op))
        })),
        build_result_ty: crate::pat::node_pat::BuildTy::Fixed(ir::node::NodeOutputType::Bool),
        outputs: crate::pat::node_pat::OutputsSpec::None,
        consumers: crate::pat::node_pat::ConsumersSpec::None,
        candidate_kind: None,
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
pub fn bool_binary_any(op_var: BoolBinaryOpVar, lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> Pat {
    let inputs = InputsSpec::fixed_maybe_commutative(lhs.into(), rhs.into(), |ctx, node| {
        match ctx.graph.graph.node_kind(node) {
            NodeKind::BoolBinaryOp(op) => is_commutative_bool_op(*op),
            _ => false,
        }
    });
    Pat::from_dyn(Arc::new(NodePat {
        kind_build: Some(Arc::new(move |ctx| {
            let op = ctx.bindings
                .get_bool_binary_op(op_var)
                .ok_or(crate::error::ErrorKind::MissingBinding("BoolBinaryOpVar"))?;
            Ok(NodeKind::BoolBinaryOp(op))
        })),
        build_result_ty: crate::pat::node_pat::BuildTy::Fixed(ir::node::NodeOutputType::Bool),
        outputs: crate::pat::node_pat::OutputsSpec::None,
        consumers: crate::pat::node_pat::ConsumersSpec::None,
        candidate_kind: None,
        kind_match: Arc::new(|ctx, node, _b| {
            matches!(ctx.graph.graph.node_kind(node), NodeKind::BoolBinaryOp(_))
        }),
        inputs,
        post_match: Some(Arc::new(move |ctx, node, b| {
            match ctx.graph.graph.node_kind(node) {
                NodeKind::BoolBinaryOp(op) => b.bind_bool_binary_op(op_var, *op),
                _ => false,
            }
        })),
        output_var: None,
        node_var: None,
    }))
}

/// Matches **any** boolean unary operation and binds the actual operator
/// variant to `op`.
pub fn bool_unary_any(op_var: BoolUnaryOpVar, operand: impl Into<Pat>) -> Pat {
    Pat::from_dyn(Arc::new(NodePat {
        kind_build: Some(Arc::new(move |ctx| {
            let op = ctx.bindings
                .get_bool_unary_op(op_var)
                .ok_or(crate::error::ErrorKind::MissingBinding("BoolUnaryOpVar"))?;
            Ok(NodeKind::BoolUnaryOp(op))
        })),
        build_result_ty: crate::pat::node_pat::BuildTy::Fixed(ir::node::NodeOutputType::Bool),
        outputs: crate::pat::node_pat::OutputsSpec::None,
        consumers: crate::pat::node_pat::ConsumersSpec::None,
        candidate_kind: None,
        kind_match: Arc::new(|ctx, node, _b| {
            matches!(ctx.graph.graph.node_kind(node), NodeKind::BoolUnaryOp(_))
        }),
        inputs: InputsSpec::fixed_ordered(vec![operand.into()]),
        post_match: Some(Arc::new(move |ctx, node, b| {
            match ctx.graph.graph.node_kind(node) {
                NodeKind::BoolUnaryOp(op) => b.bind_bool_unary_op(op_var, *op),
                _ => false,
            }
        })),
        output_var: None,
        node_var: None,
    }))
}

/// Matches **any** float binary operation and binds the actual operator
/// variant to `op`.
///
/// Commutative ops (`Add`, `Mul`) try both operand orderings automatically.
pub fn float_binary_any(
    op_var: FloatBinaryOpVar,
    lhs: impl Into<Pat>,
    rhs: impl Into<Pat>,
) -> Pat {
    let inputs = InputsSpec::fixed_maybe_commutative(lhs.into(), rhs.into(), |ctx, node| {
        match ctx.graph.graph.node_kind(node) {
            NodeKind::FloatBinaryOp(op) => is_commutative_float_op(*op),
            _ => false,
        }
    });
    Pat::from_dyn(Arc::new(NodePat {
        kind_build: Some(Arc::new(move |ctx| {
            let op = ctx.bindings
                .get_float_binary_op(op_var)
                .ok_or(crate::error::ErrorKind::MissingBinding("FloatBinaryOpVar"))?;
            Ok(NodeKind::FloatBinaryOp(op))
        })),
        build_result_ty: crate::pat::node_pat::BuildTy::InheritRoot,
        outputs: crate::pat::node_pat::OutputsSpec::None,
        consumers: crate::pat::node_pat::ConsumersSpec::None,
        candidate_kind: None,
        kind_match: Arc::new(|ctx, node, _b| {
            matches!(ctx.graph.graph.node_kind(node), NodeKind::FloatBinaryOp(_))
        }),
        inputs,
        post_match: Some(Arc::new(move |ctx, node, b| {
            match ctx.graph.graph.node_kind(node) {
                NodeKind::FloatBinaryOp(op) => b.bind_float_binary_op(op_var, *op),
                _ => false,
            }
        })),
        output_var: None,
        node_var: None,
    }))
}

/// Matches **any** float unary operation and binds the actual operator
/// variant to `op`.
pub fn float_unary_any(op_var: FloatUnaryOpVar, operand: impl Into<Pat>) -> Pat {
    Pat::from_dyn(Arc::new(NodePat {
        kind_build: Some(Arc::new(move |ctx| {
            let op = ctx.bindings
                .get_float_unary_op(op_var)
                .ok_or(crate::error::ErrorKind::MissingBinding("FloatUnaryOpVar"))?;
            Ok(NodeKind::FloatUnaryOp(op))
        })),
        build_result_ty: crate::pat::node_pat::BuildTy::InheritRoot,
        outputs: crate::pat::node_pat::OutputsSpec::None,
        consumers: crate::pat::node_pat::ConsumersSpec::None,
        candidate_kind: None,
        kind_match: Arc::new(|ctx, node, _b| {
            matches!(ctx.graph.graph.node_kind(node), NodeKind::FloatUnaryOp(_))
        }),
        inputs: InputsSpec::fixed_ordered(vec![operand.into()]),
        post_match: Some(Arc::new(move |ctx, node, b| {
            match ctx.graph.graph.node_kind(node) {
                NodeKind::FloatUnaryOp(op) => b.bind_float_unary_op(op_var, *op),
                _ => false,
            }
        })),
        output_var: None,
        node_var: None,
    }))
}

/// Matches **any** float comparison and binds the actual operator variant
/// to `op`.
///
/// No float comparison operators are currently treated as commutative, so no
/// automatic operand-swap retry is attempted.
pub fn float_cmp_any(op_var: FloatCmpOpVar, lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> Pat {
    Pat::from_dyn(Arc::new(NodePat {
        kind_build: Some(Arc::new(move |ctx| {
            let op = ctx.bindings
                .get_float_cmp_op(op_var)
                .ok_or(crate::error::ErrorKind::MissingBinding("FloatCmpOpVar"))?;
            Ok(NodeKind::FloatCmpOp(op))
        })),
        build_result_ty: crate::pat::node_pat::BuildTy::Fixed(ir::node::NodeOutputType::Bool),
        outputs: crate::pat::node_pat::OutputsSpec::None,
        consumers: crate::pat::node_pat::ConsumersSpec::None,
        candidate_kind: None,
        kind_match: Arc::new(|ctx, node, _b| {
            matches!(ctx.graph.graph.node_kind(node), NodeKind::FloatCmpOp(_))
        }),
        inputs: InputsSpec::fixed_ordered(vec![lhs.into(), rhs.into()]),
        post_match: Some(Arc::new(move |ctx, node, b| {
            match ctx.graph.graph.node_kind(node) {
                NodeKind::FloatCmpOp(op) => b.bind_float_cmp_op(op_var, *op),
                _ => false,
            }
        })),
        output_var: None,
        node_var: None,
    }))
}
