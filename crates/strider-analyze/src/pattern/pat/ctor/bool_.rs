//! Boolean binary and unary pattern constructors.
//!
//! This module is named `bool_` (trailing underscore) because `bool` is a Rust
//! primitive type and reusing it as a module name requires `mod r#bool;` /
//! `r#bool::…` at every call site, which is uglier than the suffix.
//!
//! Booleans are 1-bit integers (`I1`) in this IR: there is no separate
//! `BoolBinaryOp` / `BoolUnaryOp` node kind.  A boolean AND / OR / XOR is an
//! [`IntBinaryOp`](strider_ir::IntBinaryOp) (`And` / `Or` / `Xor`) whose output
//! is `I1`, and a logical NOT is an [`IntBinaryOp::Xor`] with the I1 all-ones
//! constant `IntConst(1)` (since the former BitNot unary-op was removed in favour
//! of `Xor(x, all_ones)`).  These constructors keep their historical `bool_*`
//! names but build/match the integer shapes at `I1`.

use strider_ir::IntBinaryOp;

use crate::pattern::pat::{BoolBinaryOpPat, Pat};

/// Matches a boolean binary operation with the given `op`.
///
/// `op` is an [`IntBinaryOp`] (booleans are `I1` integers); use `And`, `Or`,
/// or `Xor`.  Commutative ops try both orderings automatically; call
/// `.ordered()` on the returned `BoolBinaryOpPat` to disable this.  The
/// result node is typed `I1`.
pub fn bool_binary(op: IntBinaryOp, lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> BoolBinaryOpPat {
    BoolBinaryOpPat::new(op, lhs.into(), rhs.into())
}

/// Matches a boolean AND node (`IntBinaryOp::And` at `I1`).  Commutative;
/// call `.ordered()` to pin operand order.
pub fn bool_and(lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> BoolBinaryOpPat {
    BoolBinaryOpPat::new(IntBinaryOp::And, lhs.into(), rhs.into())
}

/// Matches a boolean OR node (`IntBinaryOp::Or` at `I1`).  Commutative;
/// call `.ordered()` to pin operand order.
pub fn bool_or(lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> BoolBinaryOpPat {
    BoolBinaryOpPat::new(IntBinaryOp::Or, lhs.into(), rhs.into())
}

/// Matches a boolean XOR node (`IntBinaryOp::Xor` at `I1`).  Commutative;
/// call `.ordered()` to pin operand order.
pub fn bool_xor(lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> BoolBinaryOpPat {
    BoolBinaryOpPat::new(IntBinaryOp::Xor, lhs.into(), rhs.into())
}

/// Matches a boolean NOT node — a 1-bit `Xor(x, IntConst(1)):I1` (logical
/// NOT of an `I1` value).  the former BitNot unary-op was removed in favour of
/// `Xor(x, all_ones)`; at `I1` the all-ones constant is `IntConst(1)`.
pub fn bool_not(operand: impl Into<Pat>) -> Pat {
    use crate::pattern::pat::ctor::wildcards::int_const_all_ones;
    BoolBinaryOpPat::new(IntBinaryOp::Xor, operand.into(), int_const_all_ones()).into()
}
