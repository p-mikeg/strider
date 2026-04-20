//! Float binary / unary / comparison / conversion / bitcast pattern constructors.

use ir::{FloatBinaryOp, FloatCmpOp, FloatUnaryOp};

use crate::pat::{FloatBinaryOpPat, Pat, PatKind};

/// Matches a float binary operation with the given `op`.
///
/// Commutative ops (`Add`, `Mul`) will try both operand orderings automatically.
/// Call `.ordered()` on the result to disable this.
pub fn float_binary(
    op: FloatBinaryOp,
    lhs: impl Into<Pat>,
    rhs: impl Into<Pat>,
) -> FloatBinaryOpPat {
    FloatBinaryOpPat {
        op,
        lhs: lhs.into(),
        rhs: rhs.into(),
        ordered: false,
    }
}
/// Matches a float addition node.  Commutative.
pub fn float_add(lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> FloatBinaryOpPat {
    float_binary(FloatBinaryOp::Add, lhs, rhs)
}
/// Matches a float subtraction node.  Not commutative.
pub fn float_sub(lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> FloatBinaryOpPat {
    float_binary(FloatBinaryOp::Sub, lhs, rhs)
}
/// Matches a float multiplication node.  Commutative.
pub fn float_mul(lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> FloatBinaryOpPat {
    float_binary(FloatBinaryOp::Mul, lhs, rhs)
}
/// Matches a float division node.  Not commutative.
pub fn float_div(lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> FloatBinaryOpPat {
    float_binary(FloatBinaryOp::Div, lhs, rhs)
}

/// Matches a float unary operation with the given `op`.
pub fn float_unary(op: FloatUnaryOp, operand: impl Into<Pat>) -> Pat {
    Pat::new(PatKind::FloatUnaryOp {
        op,
        operand: operand.into(),
    })
}
/// Matches a float negation node.
pub fn float_neg(operand: impl Into<Pat>) -> Pat {
    float_unary(FloatUnaryOp::Neg, operand)
}
/// Matches a float absolute-value node.
pub fn float_abs(operand: impl Into<Pat>) -> Pat {
    float_unary(FloatUnaryOp::Abs, operand)
}
/// Matches a float square-root node.
pub fn float_sqrt(operand: impl Into<Pat>) -> Pat {
    float_unary(FloatUnaryOp::Sqrt, operand)
}
/// Matches a float ceiling node.
pub fn float_ceil(operand: impl Into<Pat>) -> Pat {
    float_unary(FloatUnaryOp::Ceil, operand)
}
/// Matches a float floor node.
pub fn float_floor(operand: impl Into<Pat>) -> Pat {
    float_unary(FloatUnaryOp::Floor, operand)
}
/// Matches a float round node.
pub fn float_round(operand: impl Into<Pat>) -> Pat {
    float_unary(FloatUnaryOp::Round, operand)
}

/// Matches a float comparison node with the given `op`.
pub fn float_cmp(op: FloatCmpOp, lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> Pat {
    Pat::new(PatKind::FloatCmpOp {
        op,
        lhs: lhs.into(),
        rhs: rhs.into(),
    })
}
/// Matches a float equality comparison.
pub fn float_eq(lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> Pat {
    float_cmp(FloatCmpOp::Equal, lhs, rhs)
}
/// Matches a float not-equal comparison.
pub fn float_ne(lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> Pat {
    float_cmp(FloatCmpOp::NotEqual, lhs, rhs)
}
/// Matches a float less-than comparison.
pub fn float_lt(lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> Pat {
    float_cmp(FloatCmpOp::Less, lhs, rhs)
}
/// Matches a float less-or-equal comparison.
pub fn float_le(lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> Pat {
    float_cmp(FloatCmpOp::LessEqual, lhs, rhs)
}

/// Matches an `IntToFloat` value-conversion node.
pub fn int_to_float(operand: impl Into<Pat>) -> Pat {
    Pat::new(PatKind::IntToFloat {
        operand: operand.into(),
    })
}
/// Matches a `FloatToInt` value-conversion node.
pub fn float_to_int(operand: impl Into<Pat>) -> Pat {
    Pat::new(PatKind::FloatToInt {
        operand: operand.into(),
    })
}
/// Matches a `FloatToFloat` precision-conversion node.
pub fn float_to_float(operand: impl Into<Pat>) -> Pat {
    Pat::new(PatKind::FloatToFloat {
        operand: operand.into(),
    })
}
/// Matches an `IntBitsToFloat` bitcast node.
pub fn int_bits_to_float(operand: impl Into<Pat>) -> Pat {
    Pat::new(PatKind::IntBitsToFloat {
        operand: operand.into(),
    })
}
/// Matches a `FloatBitsToInt` bitcast node.
pub fn float_bits_to_int(operand: impl Into<Pat>) -> Pat {
    Pat::new(PatKind::FloatBitsToInt {
        operand: operand.into(),
    })
}
