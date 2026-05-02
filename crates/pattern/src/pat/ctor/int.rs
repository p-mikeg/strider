//! Integer binary, unary, and comparison pattern constructors.

use ir::{IntBinaryOp, IntCmpOp, IntUnaryOp};

use crate::macros::{decl_pat_binary_ops, decl_pat_cmp_ops, decl_pat_unary_ops};
use crate::pat::builders::{BinaryOpPat, cmp_pat, unary_pat};
use crate::pat::IntBinaryOpPat;
use crate::pat::Pat;

// ── Integer binary ops ────────────────────────────────────────────────────────

/// Matches an integer binary operation with the given `op`.
///
/// Commutative ops (`Add`, `Mul`, `And`, `Or`, `Xor`) will try both operand
/// orderings automatically.  Call `.ordered()` on the result to disable this.
pub fn int_binary(op: IntBinaryOp, lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> IntBinaryOpPat {
    BinaryOpPat::new(op, lhs.into(), rhs.into())
}

decl_pat_binary_ops!(int_binary, IntBinaryOp, IntBinaryOpPat, [
    /// Matches an unsigned addition node (`lhs + rhs`).  Commutative.
    (add, Add),
    /// Matches an unsigned subtraction node (`lhs - rhs`).  Not commutative.
    (sub, Sub),
    /// Matches an unsigned multiplication node.  Commutative.
    (mul, Mul),
    /// Matches an unsigned division node.  Not commutative.
    (div, Div),
    /// Matches a signed division node.  Not commutative.
    (sdiv, Sdiv),
    /// Matches an unsigned remainder node.  Not commutative.
    (rem, Rem),
    /// Matches a signed remainder node.  Not commutative.
    (srem, Srem),
    /// Matches a bitwise AND node.  Commutative.
    (and, And),
    /// Matches a bitwise OR node.  Commutative.
    (or, Or),
    /// Matches a bitwise XOR node.  Commutative.
    (xor, Xor),
    /// Matches a logical left-shift node.  Not commutative.
    (shl, ShiftLeft),
    /// Matches a logical right-shift node.  Not commutative.
    (shr, ShiftRight),
    /// Matches an arithmetic (signed) right-shift node.  Not commutative.
    (sshr, SShiftRight),
]);

// ── Integer unary ops ─────────────────────────────────────────────────────────

/// Matches an integer unary operation with the given `op`.
pub fn int_unary(op: IntUnaryOp, operand: impl Into<Pat>) -> Pat {
    unary_pat(op, operand.into())
}

decl_pat_unary_ops!(int_unary, IntUnaryOp, Pat, [
    /// Matches a two's-complement negation node (`-operand`).
    (neg, Neg),
    /// Matches a bitwise complement node (`~operand`).
    (bit_not, BitNot),
]);

// ── Integer comparisons (→ Bool) ──────────────────────────────────────────────

/// Matches an integer comparison node with the given `op`.
///
/// For commutative ops (`Equal`, `Carry`, `Scarry`), both operand orderings
/// are tried automatically.
pub fn int_cmp(op: IntCmpOp, lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> Pat {
    cmp_pat(op, lhs.into(), rhs.into())
}

decl_pat_cmp_ops!(int_cmp, IntCmpOp, Pat, [
    /// Matches an unsigned equality comparison (`lhs == rhs`).
    (int_eq, Equal),
    /// Matches an unsigned less-than comparison (`lhs < rhs`).
    (int_lt, Less),
    /// Matches an unsigned less-or-equal comparison (`lhs <= rhs`).
    (int_le, LessEqual),
    /// Matches a signed less-than comparison.
    (int_slt, Sless),
    /// Matches a signed less-or-equal comparison.
    (int_sle, SlessEqual),
    /// Matches an unsigned addition carry-out check.
    (int_carry, Carry),
    /// Matches a signed addition overflow check.
    (int_scarry, Scarry),
    /// Matches a signed subtraction borrow check.
    (int_sborrow, Sborrow),
]);
