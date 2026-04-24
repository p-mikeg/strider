//! Float binary / unary / comparison / conversion / bitcast pattern constructors.

use std::sync::Arc;

use ir::node::NodeKind;
use ir::{FloatBinaryOp, FloatCmpOp, FloatUnaryOp};

use crate::macros::{decl_pat_binary_ops, decl_pat_cmp_ops, decl_pat_unary_ops};
use crate::pat::FloatBinaryOpPat;
use crate::pat::Pat;
use crate::pat::node_pat::{InputsSpec, NodePat};

/// Matches a float binary operation with the given `op`.
///
/// Commutative ops (`Add`, `Mul`) will try both operand orderings automatically.
/// Call `.ordered()` on the result to disable this.
pub fn float_binary(
    op: FloatBinaryOp,
    lhs: impl Into<Pat>,
    rhs: impl Into<Pat>,
) -> FloatBinaryOpPat {
    FloatBinaryOpPat::new(op, lhs.into(), rhs.into())
}

decl_pat_binary_ops!(float_binary, FloatBinaryOp, FloatBinaryOpPat, [
    /// Matches a float addition node.  Commutative.
    (float_add, Add),
    /// Matches a float subtraction node.  Not commutative.
    (float_sub, Sub),
    /// Matches a float multiplication node.  Commutative.
    (float_mul, Mul),
    /// Matches a float division node.  Not commutative.
    (float_div, Div),
]);

/// Matches a float unary operation with the given `op`.
pub fn float_unary(op: FloatUnaryOp, operand: impl Into<Pat>) -> Pat {
    Pat::from_dyn(Arc::new(NodePat {
        outputs: crate::pat::node_pat::OutputsSpec::None,
        consumers: crate::pat::node_pat::ConsumersSpec::None,
        candidate_kind: None,
        kind_match: Arc::new(move |ctx, node, _b| {
            matches!(ctx.graph.graph.node_kind(node), NodeKind::FloatUnaryOp(x) if *x == op)
        }),
        inputs: InputsSpec::fixed_ordered(vec![operand.into()]),
        post_match: None,
        output_var: None,
        node_var: None,
    }))
}

decl_pat_unary_ops!(float_unary, FloatUnaryOp, Pat, [
    /// Matches a float negation node.
    (float_neg, Neg),
    /// Matches a float absolute-value node.
    (float_abs, Abs),
    /// Matches a float square-root node.
    (float_sqrt, Sqrt),
    /// Matches a float ceiling node.
    (float_ceil, Ceil),
    /// Matches a float floor node.
    (float_floor, Floor),
    /// Matches a float round node.
    (float_round, Round),
]);

/// Matches a float comparison node with the given `op`.
pub fn float_cmp(op: FloatCmpOp, lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> Pat {
    Pat::from_dyn(Arc::new(NodePat {
        outputs: crate::pat::node_pat::OutputsSpec::None,
        consumers: crate::pat::node_pat::ConsumersSpec::None,
        candidate_kind: None,
        kind_match: Arc::new(move |ctx, node, _b| {
            matches!(ctx.graph.graph.node_kind(node), NodeKind::FloatCmpOp(x) if *x == op)
        }),
        inputs: InputsSpec::fixed_ordered(vec![lhs.into(), rhs.into()]),
        post_match: None,
        output_var: None,
        node_var: None,
    }))
}

decl_pat_cmp_ops!(float_cmp, FloatCmpOp, Pat, [
    /// Matches a float equality comparison.
    (float_eq, Equal),
    /// Matches a float not-equal comparison.
    (float_ne, NotEqual),
    /// Matches a float less-than comparison.
    (float_lt, Less),
    /// Matches a float less-or-equal comparison.
    (float_le, LessEqual),
]);

/// Matches an `IntToFloat` value-conversion node.
pub fn int_to_float(operand: impl Into<Pat>) -> Pat {
    Pat::from_dyn(Arc::new(NodePat {
        outputs: crate::pat::node_pat::OutputsSpec::None,
        consumers: crate::pat::node_pat::ConsumersSpec::None,
        candidate_kind: None,
        kind_match: Arc::new(|ctx, node, _b| {
            matches!(ctx.graph.graph.node_kind(node), NodeKind::IntToFloat)
        }),
        inputs: InputsSpec::fixed_ordered(vec![operand.into()]),
        post_match: None,
        output_var: None,
        node_var: None,
    }))
}
/// Matches a `FloatToInt` value-conversion node.
pub fn float_to_int(operand: impl Into<Pat>) -> Pat {
    Pat::from_dyn(Arc::new(NodePat {
        outputs: crate::pat::node_pat::OutputsSpec::None,
        consumers: crate::pat::node_pat::ConsumersSpec::None,
        candidate_kind: None,
        kind_match: Arc::new(|ctx, node, _b| {
            matches!(ctx.graph.graph.node_kind(node), NodeKind::FloatToInt)
        }),
        inputs: InputsSpec::fixed_ordered(vec![operand.into()]),
        post_match: None,
        output_var: None,
        node_var: None,
    }))
}
/// Matches a `FloatToFloat` precision-conversion node.
pub fn float_to_float(operand: impl Into<Pat>) -> Pat {
    Pat::from_dyn(Arc::new(NodePat {
        outputs: crate::pat::node_pat::OutputsSpec::None,
        consumers: crate::pat::node_pat::ConsumersSpec::None,
        candidate_kind: None,
        kind_match: Arc::new(|ctx, node, _b| {
            matches!(ctx.graph.graph.node_kind(node), NodeKind::FloatToFloat)
        }),
        inputs: InputsSpec::fixed_ordered(vec![operand.into()]),
        post_match: None,
        output_var: None,
        node_var: None,
    }))
}
/// Matches an `IntBitsToFloat` bitcast node.
pub fn int_bits_to_float(operand: impl Into<Pat>) -> Pat {
    Pat::from_dyn(Arc::new(NodePat {
        outputs: crate::pat::node_pat::OutputsSpec::None,
        consumers: crate::pat::node_pat::ConsumersSpec::None,
        candidate_kind: None,
        kind_match: Arc::new(|ctx, node, _b| {
            matches!(ctx.graph.graph.node_kind(node), NodeKind::IntBitsToFloat)
        }),
        inputs: InputsSpec::fixed_ordered(vec![operand.into()]),
        post_match: None,
        output_var: None,
        node_var: None,
    }))
}
/// Matches a `FloatBitsToInt` bitcast node.
pub fn float_bits_to_int(operand: impl Into<Pat>) -> Pat {
    Pat::from_dyn(Arc::new(NodePat {
        outputs: crate::pat::node_pat::OutputsSpec::None,
        consumers: crate::pat::node_pat::ConsumersSpec::None,
        candidate_kind: None,
        kind_match: Arc::new(|ctx, node, _b| {
            matches!(ctx.graph.graph.node_kind(node), NodeKind::FloatBitsToInt)
        }),
        inputs: InputsSpec::fixed_ordered(vec![operand.into()]),
        post_match: None,
        output_var: None,
        node_var: None,
    }))
}
