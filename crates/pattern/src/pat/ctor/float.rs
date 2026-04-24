//! Float binary / unary / comparison / conversion / bitcast pattern constructors.

use ir::node::NodeKind;
use ir::{FloatBinaryOp, FloatCmpOp, FloatUnaryOp};

use crate::macros::{decl_pat_binary_ops, decl_pat_cmp_ops, decl_pat_unary_ops};
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
pub fn float_cmp(op: FloatCmpOp, lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> Pat {
    NodePat::matcher(
        KindSpec::Exact(NodeKind::FloatCmpOp(op)),
        InputsSpec::fixed_ordered(vec![lhs.into(), rhs.into()]),
    )
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

/// Helper for the five unit-variant float conversions (IntToFloat,
/// FloatToInt, FloatToFloat, IntBitsToFloat, FloatBitsToInt).
fn unit_conv(build_kind: NodeKind, operand: impl Into<Pat>) -> Pat {
    NodePat::matcher(
        KindSpec::Exact(build_kind),
        InputsSpec::fixed_ordered(vec![operand.into()]),
    )
    .with_build_exact(build_kind, BuildTy::InheritRoot)
    .into_pat()
}

/// Matches an `IntToFloat` value-conversion node.
pub fn int_to_float(operand: impl Into<Pat>) -> Pat {
    unit_conv(NodeKind::IntToFloat, operand)
}
/// Matches a `FloatToInt` value-conversion node.
pub fn float_to_int(operand: impl Into<Pat>) -> Pat {
    unit_conv(NodeKind::FloatToInt, operand)
}
/// Matches a `FloatToFloat` precision-conversion node.
pub fn float_to_float(operand: impl Into<Pat>) -> Pat {
    unit_conv(NodeKind::FloatToFloat, operand)
}
/// Matches an `IntBitsToFloat` bitcast node.
pub fn int_bits_to_float(operand: impl Into<Pat>) -> Pat {
    unit_conv(NodeKind::IntBitsToFloat, operand)
}
/// Matches a `FloatBitsToInt` bitcast node.
pub fn float_bits_to_int(operand: impl Into<Pat>) -> Pat {
    unit_conv(NodeKind::FloatBitsToInt, operand)
}
