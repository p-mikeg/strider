use ir::{BoolBinaryOp, FloatBinaryOp, FloatCmpOp, IntBinaryOp, IntCmpOp};

// ── Commutativity helpers ─────────────────────────────────────────────────────

pub(crate) fn is_commutative_int_op(op: IntBinaryOp) -> bool {
    matches!(
        op,
        IntBinaryOp::Add | IntBinaryOp::Mul | IntBinaryOp::And | IntBinaryOp::Or | IntBinaryOp::Xor
    )
}

pub(crate) fn is_commutative_bool_op(op: BoolBinaryOp) -> bool {
    matches!(op, BoolBinaryOp::And | BoolBinaryOp::Or | BoolBinaryOp::Xor)
}

pub(crate) fn is_commutative_float_op(op: FloatBinaryOp) -> bool {
    matches!(op, FloatBinaryOp::Add | FloatBinaryOp::Mul)
}

/// `Equal` is symmetric by definition.  `Carry(l, r)` and `Scarry(l, r)` ask
/// whether the addition `l + r` overflows (unsigned / signed respectively);
/// since addition commutes so do these two comparisons.  `Less` / `LessEqual`
/// / `Sless` / `SlessEqual` are directional, and `Borrow` / `Sborrow` encode
/// subtraction — all non-commutative, and intentionally excluded.
pub(crate) fn is_commutative_int_cmp_op(op: IntCmpOp) -> bool {
    matches!(op, IntCmpOp::Equal | IntCmpOp::Carry | IntCmpOp::Scarry)
}

/// `Equal` and `NotEqual` are symmetric for IEEE 754 (the comparison
/// returns the same result regardless of operand order, including for
/// NaN inputs — both orderings yield `false` / `true` consistently).
/// `Less` / `LessEqual` are directional and intentionally excluded.
pub(crate) fn is_commutative_float_cmp_op(op: FloatCmpOp) -> bool {
    matches!(op, FloatCmpOp::Equal | FloatCmpOp::NotEqual)
}
