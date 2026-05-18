use strider_ir::{BoolBinaryOp, FloatBinaryOp, FloatCmpOp, IntBinaryOp, IntCmpOp};

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
/// / `Sless` / `SlessEqual` are directional, and `Sborrow` encodes signed
/// subtraction overflow — all non-commutative, and intentionally excluded.
pub(crate) fn is_commutative_int_cmp_op(op: IntCmpOp) -> bool {
    matches!(op, IntCmpOp::Equal | IntCmpOp::Carry | IntCmpOp::Scarry)
}

/// `Equal` is symmetric for IEEE 754 (yields the same result regardless
/// of operand order, including for NaN inputs).  `Less` is directional.
/// `NotEqual` and `LessEqual` are not primitives in this IR — they are
/// lowered at lift time to compositions of `Equal` and `Less` (see
/// `pcode_lift::value::float`).
pub(crate) fn is_commutative_float_cmp_op(op: FloatCmpOp) -> bool {
    matches!(op, FloatCmpOp::Equal)
}
