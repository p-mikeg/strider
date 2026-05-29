//! Float binary / unary / comparison / conversion / bitcast pattern constructors.

use strider_ir::node::NodeKind;
use strider_ir::{FloatBinaryOp, FloatCmpOp, FloatUnaryOp};

use crate::pattern::macros::{decl_pat_binary_ops, decl_pat_cmp_ops, decl_pat_unary_ops};
use crate::pattern::pat::builders::{BinaryOpPat, cmp_pat, unary_pat};
use crate::pattern::pat::FloatBinaryOpPat;
use crate::pattern::pat::Pat;
use crate::pattern::pat::node_pat::BuildTy;

/// Matches a float binary operation with the given `op`.
///
/// Commutative ops (`Add`, `Mul`) will try both operand orderings automatically.
/// Call `.ordered()` on the result to disable this.
pub fn float_binary(
    op: FloatBinaryOp,
    lhs: impl Into<Pat>,
    rhs: impl Into<Pat>,
) -> FloatBinaryOpPat {
    BinaryOpPat::new(op, lhs.into(), rhs.into())
}

decl_pat_binary_ops!(float_binary, FloatBinaryOp, FloatBinaryOpPat, [
    /// Matches a float addition node.  Commutative.
    (float_add, Add),
    /// Matches a float multiplication node.  Commutative.
    (float_mul, Mul),
    /// Matches a float division node.  Not commutative.
    (float_div, Div),
]);

/// Matches a float subtraction `lhs - rhs`.
///
/// `FloatBinaryOp::Sub` is not a primitive; pcode-lift lowers
/// `FloatSub(a, b)` at lift time to `FloatAdd(a, FloatUnaryOp::Neg(b))`.
/// This constructor produces the lowered shape so `float_sub(a, b)`
/// matches the same IR `a - b` produces.
pub fn float_sub(lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> Pat {
    use crate::pattern::pat::builders::unary_pat;
    let neg_rhs = unary_pat(strider_ir::FloatUnaryOp::Neg, rhs.into());
    BinaryOpPat::new(FloatBinaryOp::Add, lhs.into(), neg_rhs).into()
}

/// Matches a float unary operation with the given `op`.
pub fn float_unary(op: FloatUnaryOp, operand: impl Into<Pat>) -> Pat {
    unary_pat(op, operand.into())
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
/// `Equal` is commutative under IEEE 754 (yields the same result regardless
/// of operand order, including for NaN inputs); both operand orderings are
/// tried automatically.  `Less` is directional — the matcher only tries the
/// stated order.  `NotEqual` and `LessEqual` are not primitives in this IR
/// (lifter lowers them at lift time); use `float_ne` / `float_le` for those.
pub fn float_cmp(op: FloatCmpOp, lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> Pat {
    cmp_pat(op, lhs.into(), rhs.into())
}

decl_pat_cmp_ops!(float_cmp, FloatCmpOp, Pat, [
    /// Matches a float equality comparison.
    (float_eq, Equal),
    /// Matches a float less-than comparison.
    (float_lt, Less),
]);

/// Matches a float not-equal comparison `lhs != rhs`.
///
/// `FloatCmpOp::NotEqual` is not a primitive; pcode-lift lowers
/// `FloatNotEqual(a, b)` at lift time to `BitNot(FloatEqual(a, b))` at `I1`.
/// This constructor produces the lowered shape directly.
pub fn float_ne(lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> Pat {
    use crate::pattern::pat::ctor::bool_::bool_not;
    bool_not(cmp_pat(FloatCmpOp::Equal, lhs.into(), rhs.into()))
}

/// Matches a float less-or-equal comparison `lhs <= rhs`.
///
/// `FloatCmpOp::LessEqual` is not a primitive; pcode-lift lowers
/// `FloatLessEqual(a, b)` at lift time to
/// `Or(FloatLess(a, b), FloatEqual(a, b))` — NaN-aware (cannot use
/// the operand-swap-and-negate trick because IEEE 754 `<=` is false
/// on NaN, while `BitNot(Less(b, a))` would be true).
pub fn float_le(lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> Pat {
    use crate::pattern::pat::ctor::bool_::bool_or;
    let lhs_p: Pat = lhs.into();
    let rhs_p: Pat = rhs.into();
    bool_or(
        cmp_pat(FloatCmpOp::Less, lhs_p.clone(), rhs_p.clone()),
        cmp_pat(FloatCmpOp::Equal, lhs_p, rhs_p),
    )
    .into()
}

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
