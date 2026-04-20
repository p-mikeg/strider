//! Variant-agnostic ("`*_any`") op constructors.
//!
//! These patterns match **any** variant of an op family (int binary, bool
//! unary, …) and bind the actual operator variant to a typed capture variable.

use crate::pat::{Pat, PatKind};
use crate::var::{
    BoolBinaryOpVar, BoolUnaryOpVar, FloatBinaryOpVar, FloatCmpOpVar, FloatUnaryOpVar,
    IntBinaryOpVar, IntCmpOpVar, IntUnaryOpVar,
};

/// Matches **any** integer binary operation and binds the actual operator
/// variant to `op`.
///
/// Commutative ops (`Add`, `Mul`, `And`, `Or`, `Xor`) will try both operand
/// orderings automatically unless the returned `Pat` is wrapped via a custom
/// ordered pattern.  Because `int_binary_any` returns a `Pat` directly rather
/// than a builder, there is no `.ordered()` method; use `ordered: true` at
/// the `PatKind` level if you need to construct one manually, or build the
/// `PatKind::IntBinaryAny` directly.
pub fn int_binary_any(op: IntBinaryOpVar, lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> Pat {
    Pat::new(PatKind::IntBinaryAny {
        op,
        lhs: lhs.into(),
        rhs: rhs.into(),
        ordered: false,
    })
}

/// Matches **any** integer unary operation and binds the actual operator
/// variant to `op`.
pub fn int_unary_any(op: IntUnaryOpVar, operand: impl Into<Pat>) -> Pat {
    Pat::new(PatKind::IntUnaryAny {
        op,
        operand: operand.into(),
    })
}

/// Matches **any** integer comparison and binds the actual operator variant
/// to `op`.
///
/// Commutative comparisons (`Equal`, `Carry`, `Scarry`) try both operand
/// orderings automatically.
pub fn int_cmp_any(op: IntCmpOpVar, lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> Pat {
    Pat::new(PatKind::IntCmpAny {
        op,
        lhs: lhs.into(),
        rhs: rhs.into(),
        ordered: false,
    })
}

/// Matches **any** boolean binary operation and binds the actual operator
/// variant to `op`.
///
/// Commutative ops (`And`, `Or`, `Xor`) try both operand orderings
/// automatically.
pub fn bool_binary_any(op: BoolBinaryOpVar, lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> Pat {
    Pat::new(PatKind::BoolBinaryAny {
        op,
        lhs: lhs.into(),
        rhs: rhs.into(),
        ordered: false,
    })
}

/// Matches **any** boolean unary operation and binds the actual operator
/// variant to `op`.
pub fn bool_unary_any(op: BoolUnaryOpVar, operand: impl Into<Pat>) -> Pat {
    Pat::new(PatKind::BoolUnaryAny {
        op,
        operand: operand.into(),
    })
}

/// Matches **any** float binary operation and binds the actual operator
/// variant to `op`.
///
/// Commutative ops (`Add`, `Mul`) try both operand orderings automatically.
pub fn float_binary_any(op: FloatBinaryOpVar, lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> Pat {
    Pat::new(PatKind::FloatBinaryAny {
        op,
        lhs: lhs.into(),
        rhs: rhs.into(),
        ordered: false,
    })
}

/// Matches **any** float unary operation and binds the actual operator
/// variant to `op`.
pub fn float_unary_any(op: FloatUnaryOpVar, operand: impl Into<Pat>) -> Pat {
    Pat::new(PatKind::FloatUnaryAny {
        op,
        operand: operand.into(),
    })
}

/// Matches **any** float comparison and binds the actual operator variant
/// to `op`.
///
/// No float comparison operators are currently treated as commutative, so no
/// automatic operand-swap retry is attempted.
pub fn float_cmp_any(op: FloatCmpOpVar, lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> Pat {
    Pat::new(PatKind::FloatCmpAny {
        op,
        lhs: lhs.into(),
        rhs: rhs.into(),
        ordered: false,
    })
}
