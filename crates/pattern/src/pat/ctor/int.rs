//! Integer binary, unary, and comparison pattern constructors.

use ir::{IntBinaryOp, IntCmpOp, IntUnaryOp};

use crate::pat::{IntBinaryOpPat, Pat, PatKind};

// ── Integer binary ops ────────────────────────────────────────────────────────

/// Matches an integer binary operation with the given `op`.
///
/// Commutative ops (`Add`, `Mul`, `And`, `Or`, `Xor`) will try both operand
/// orderings automatically.  Call `.ordered()` on the result to disable this.
pub fn int_binary(op: IntBinaryOp, lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> IntBinaryOpPat {
    IntBinaryOpPat {
        op,
        lhs: lhs.into(),
        rhs: rhs.into(),
        ordered: false,
    }
}
/// Matches an unsigned addition node (`lhs + rhs`).  Commutative.
pub fn add(lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> IntBinaryOpPat {
    int_binary(IntBinaryOp::Add, lhs, rhs)
}
/// Matches an unsigned subtraction node (`lhs - rhs`).  Not commutative.
pub fn sub(lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> IntBinaryOpPat {
    int_binary(IntBinaryOp::Sub, lhs, rhs)
}
/// Matches an unsigned multiplication node.  Commutative.
pub fn mul(lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> IntBinaryOpPat {
    int_binary(IntBinaryOp::Mul, lhs, rhs)
}
/// Matches an unsigned division node.  Not commutative.
pub fn div(lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> IntBinaryOpPat {
    int_binary(IntBinaryOp::Div, lhs, rhs)
}
/// Matches a signed division node.  Not commutative.
pub fn sdiv(lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> IntBinaryOpPat {
    int_binary(IntBinaryOp::Sdiv, lhs, rhs)
}
/// Matches an unsigned remainder node.  Not commutative.
pub fn rem(lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> IntBinaryOpPat {
    int_binary(IntBinaryOp::Rem, lhs, rhs)
}
/// Matches a signed remainder node.  Not commutative.
pub fn srem(lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> IntBinaryOpPat {
    int_binary(IntBinaryOp::Srem, lhs, rhs)
}
/// Matches a bitwise AND node.  Commutative.
pub fn and(lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> IntBinaryOpPat {
    int_binary(IntBinaryOp::And, lhs, rhs)
}
/// Matches a bitwise OR node.  Commutative.
pub fn or(lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> IntBinaryOpPat {
    int_binary(IntBinaryOp::Or, lhs, rhs)
}
/// Matches a bitwise XOR node.  Commutative.
pub fn xor(lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> IntBinaryOpPat {
    int_binary(IntBinaryOp::Xor, lhs, rhs)
}
/// Matches a logical left-shift node.  Not commutative.
pub fn shl(lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> IntBinaryOpPat {
    int_binary(IntBinaryOp::ShiftLeft, lhs, rhs)
}
/// Matches a logical right-shift node.  Not commutative.
pub fn shr(lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> IntBinaryOpPat {
    int_binary(IntBinaryOp::ShiftRight, lhs, rhs)
}
/// Matches an arithmetic (signed) right-shift node.  Not commutative.
pub fn sshr(lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> IntBinaryOpPat {
    int_binary(IntBinaryOp::SShiftRight, lhs, rhs)
}

// ── Integer unary ops ─────────────────────────────────────────────────────────

/// Matches an integer unary operation with the given `op`.
pub fn int_unary(op: IntUnaryOp, operand: impl Into<Pat>) -> Pat {
    Pat::new(PatKind::IntUnaryOp {
        op,
        operand: operand.into(),
    })
}
/// Matches an integer negation node (`-operand`).
pub fn neg(operand: impl Into<Pat>) -> Pat {
    int_unary(IntUnaryOp::Neg, operand)
}
/// Matches a bitwise complement node (`~operand`).
pub fn not(operand: impl Into<Pat>) -> Pat {
    int_unary(IntUnaryOp::Not, operand)
}

// ── Integer comparisons (→ Bool) ──────────────────────────────────────────────

/// Matches an integer comparison node with the given `op`.
///
/// For commutative ops (`Equal`, `Carry`, `Scarry`), both operand orderings
/// are tried automatically.  Use `int_cmp_ordered` to disable this.
pub fn int_cmp(op: IntCmpOp, lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> Pat {
    Pat::new(PatKind::IntCmpOp {
        op,
        lhs: lhs.into(),
        rhs: rhs.into(),
        ordered: false,
    })
}
/// Matches an unsigned equality comparison (`lhs == rhs`).
pub fn int_eq(lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> Pat {
    int_cmp(IntCmpOp::Equal, lhs, rhs)
}
/// Matches an unsigned less-than comparison (`lhs < rhs`).
pub fn int_lt(lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> Pat {
    int_cmp(IntCmpOp::Less, lhs, rhs)
}
/// Matches an unsigned less-or-equal comparison (`lhs <= rhs`).
pub fn int_le(lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> Pat {
    int_cmp(IntCmpOp::LessEqual, lhs, rhs)
}
/// Matches a signed less-than comparison.
pub fn int_slt(lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> Pat {
    int_cmp(IntCmpOp::Sless, lhs, rhs)
}
/// Matches a signed less-or-equal comparison.
pub fn int_sle(lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> Pat {
    int_cmp(IntCmpOp::SlessEqual, lhs, rhs)
}
/// Matches an unsigned addition carry-out check.
pub fn int_carry(lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> Pat {
    int_cmp(IntCmpOp::Carry, lhs, rhs)
}
/// Matches a signed addition overflow check.
pub fn int_scarry(lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> Pat {
    int_cmp(IntCmpOp::Scarry, lhs, rhs)
}
/// Matches a signed subtraction borrow check.
pub fn int_sborrow(lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> Pat {
    int_cmp(IntCmpOp::Sborrow, lhs, rhs)
}
