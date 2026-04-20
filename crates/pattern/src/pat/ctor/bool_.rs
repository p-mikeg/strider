//! Boolean binary and unary pattern constructors.
//!
//! This module is named `bool_` (trailing underscore) because `bool` is a Rust
//! primitive type and reusing it as a module name requires `mod r#bool;` /
//! `r#bool::…` at every call site, which is uglier than the suffix.  Mirrors
//! the same convention used in `crates/pattern/src/matcher/data/bool_.rs`.

use ir::{BoolBinaryOp, BoolUnaryOp};

use crate::pat::{BoolBinaryOpPat, Pat, PatKind};

/// Matches a boolean binary operation with the given `op`.
///
/// Commutative ops (`And`, `Or`, `Xor`) try both orderings automatically.
pub fn bool_binary(op: BoolBinaryOp, lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> BoolBinaryOpPat {
    BoolBinaryOpPat {
        op,
        lhs: lhs.into(),
        rhs: rhs.into(),
        ordered: false,
    }
}
/// Matches a boolean AND node.  Commutative.
pub fn bool_and(lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> BoolBinaryOpPat {
    bool_binary(BoolBinaryOp::And, lhs, rhs)
}
/// Matches a boolean OR node.  Commutative.
pub fn bool_or(lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> BoolBinaryOpPat {
    bool_binary(BoolBinaryOp::Or, lhs, rhs)
}
/// Matches a boolean XOR node.  Commutative.
pub fn bool_xor(lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> BoolBinaryOpPat {
    bool_binary(BoolBinaryOp::Xor, lhs, rhs)
}
/// Matches a boolean unary operation with the given `op`.
pub fn bool_unary(op: BoolUnaryOp, operand: impl Into<Pat>) -> Pat {
    Pat::new(PatKind::BoolUnaryOp {
        op,
        operand: operand.into(),
    })
}
/// Matches a boolean NOT node.
pub fn bool_not(operand: impl Into<Pat>) -> Pat {
    bool_unary(BoolUnaryOp::Neg, operand)
}
