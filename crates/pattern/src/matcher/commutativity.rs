use ir::{BoolBinaryOp, FloatBinaryOp, IntBinaryOp, IntCmpOp};

// ── Commutativity helpers ─────────────────────────────────────────────────────

pub(super) fn is_commutative_int_op(op: IntBinaryOp) -> bool {
    matches!(
        op,
        IntBinaryOp::Add | IntBinaryOp::Mul | IntBinaryOp::And | IntBinaryOp::Or | IntBinaryOp::Xor
    )
}

pub(super) fn is_commutative_bool_op(op: BoolBinaryOp) -> bool {
    matches!(op, BoolBinaryOp::And | BoolBinaryOp::Or | BoolBinaryOp::Xor)
}

pub(super) fn is_commutative_float_op(op: FloatBinaryOp) -> bool {
    matches!(op, FloatBinaryOp::Add | FloatBinaryOp::Mul)
}

pub(super) fn is_commutative_int_cmp_op(op: IntCmpOp) -> bool {
    matches!(op, IntCmpOp::Equal | IntCmpOp::Carry | IntCmpOp::Scarry)
}
