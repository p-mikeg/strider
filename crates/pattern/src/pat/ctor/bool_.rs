//! Boolean binary and unary pattern constructors.
//!
//! This module is named `bool_` (trailing underscore) because `bool` is a Rust
//! primitive type and reusing it as a module name requires `mod r#bool;` /
//! `r#bool::…` at every call site, which is uglier than the suffix.  Mirrors
//! the same convention used in `crates/pattern/src/matcher/data/bool_.rs`.

use ir::{BoolBinaryOp, BoolUnaryOp};

use crate::macros::{decl_pat_binary_ops, decl_pat_unary_ops};
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

decl_pat_binary_ops!(bool_binary, BoolBinaryOp, BoolBinaryOpPat, [
    /// Matches a boolean AND node.  Commutative.
    (bool_and, And),
    /// Matches a boolean OR node.  Commutative.
    (bool_or, Or),
    /// Matches a boolean XOR node.  Commutative.
    (bool_xor, Xor),
]);

/// Matches a boolean unary operation with the given `op`.
pub fn bool_unary(op: BoolUnaryOp, operand: impl Into<Pat>) -> Pat {
    Pat::new(PatKind::BoolUnaryOp {
        op,
        operand: operand.into(),
    })
}

decl_pat_unary_ops!(bool_unary, BoolUnaryOp, Pat, [
    /// Matches a boolean NOT node.
    (bool_not, Neg),
]);
