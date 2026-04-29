//! Float binary / unary / comparison / conversion / bitcast pattern constructors.

use ir::node::NodeKind;
use ir::{FloatBinaryOp, FloatCmpOp, FloatUnaryOp};

use crate::macros::{decl_pat_binary_ops, decl_pat_cmp_ops, decl_pat_unary_ops};
use crate::matcher::commutativity::is_commutative_float_cmp_op;
use crate::pat::FloatBinaryOpPat;
use crate::pat::Pat;
use crate::pat::node_pat::{BuildTy, InputsSpec, KindSpec, NodePat};

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
    NodePat::matcher(
        KindSpec::Exact(NodeKind::FloatUnaryOp(op)),
        InputsSpec::fixed_ordered(vec![operand.into()]),
    )
    .with_build_exact(NodeKind::FloatUnaryOp(op), BuildTy::InheritRoot)
    .into_pat()
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
///
/// For commutative ops (`Equal`, `NotEqual`), both operand orderings are
/// tried automatically.
pub fn float_cmp(op: FloatCmpOp, lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> Pat {
    let inputs = if is_commutative_float_cmp_op(op) {
        InputsSpec::fixed_commutative(lhs.into(), rhs.into())
    } else {
        InputsSpec::fixed_ordered(vec![lhs.into(), rhs.into()])
    };
    NodePat::matcher(KindSpec::Exact(NodeKind::FloatCmpOp(op)), inputs)
        .with_build_exact(NodeKind::FloatCmpOp(op), BuildTy::Fixed(ir::node::NodeOutputType::Bool))
        .into_pat()
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
    super::casts::unary_node(NodeKind::IntToFloat, BuildTy::InheritRoot, operand)
}
/// Matches a `FloatToInt` value-conversion node.
pub fn float_to_int(operand: impl Into<Pat>) -> Pat {
    super::casts::unary_node(NodeKind::FloatToInt, BuildTy::InheritRoot, operand)
}
/// Matches a `FloatToFloat` precision-conversion node.
pub fn float_to_float(operand: impl Into<Pat>) -> Pat {
    super::casts::unary_node(NodeKind::FloatToFloat, BuildTy::InheritRoot, operand)
}
/// Matches an `IntBitsToFloat` bitcast node.
pub fn int_bits_to_float(operand: impl Into<Pat>) -> Pat {
    super::casts::unary_node(NodeKind::IntBitsToFloat, BuildTy::InheritRoot, operand)
}
/// Matches a `FloatBitsToInt` bitcast node.
pub fn float_bits_to_int(operand: impl Into<Pat>) -> Pat {
    super::casts::unary_node(NodeKind::FloatBitsToInt, BuildTy::InheritRoot, operand)
}
