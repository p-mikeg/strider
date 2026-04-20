//! Cast / coercion / bit-width-change pattern constructors.

use ir::ExtendOp;

use crate::pat::{Pat, PatKind};

/// Matches a `CastToBool` node (non-zero integer → `true`).
pub fn cast_to_bool(operand: impl Into<Pat>) -> Pat {
    Pat::new(PatKind::CastToBool {
        operand: operand.into(),
    })
}
/// Matches a `CastToInt` node (`bool` → `0` or `1`).
pub fn cast_to_int(operand: impl Into<Pat>) -> Pat {
    Pat::new(PatKind::CastToInt {
        operand: operand.into(),
    })
}
/// Matches a `CastToFloat` generic-cast node.
pub fn cast_to_float(operand: impl Into<Pat>) -> Pat {
    Pat::new(PatKind::CastToFloat {
        operand: operand.into(),
    })
}
/// Matches a `Truncate` node (narrows an integer to fewer bits).
pub fn truncate(operand: impl Into<Pat>) -> Pat {
    Pat::new(PatKind::Truncate {
        operand: operand.into(),
    })
}
/// Matches an `Extend` node with the given extension kind.
pub fn extend(op: ExtendOp, operand: impl Into<Pat>) -> Pat {
    Pat::new(PatKind::Extend {
        op,
        operand: operand.into(),
    })
}
/// Matches a zero-extension node.
pub fn zero_extend(operand: impl Into<Pat>) -> Pat {
    extend(ExtendOp::ZeroExtend, operand)
}
/// Matches a sign-extension node.
pub fn sign_extend(operand: impl Into<Pat>) -> Pat {
    extend(ExtendOp::SignExtend, operand)
}
/// Matches a popcount (count-set-bits) node.
pub fn popcount(operand: impl Into<Pat>) -> Pat {
    Pat::new(PatKind::Popcount {
        operand: operand.into(),
    })
}
/// Matches a leading-zero-count node.
pub fn lzcount(operand: impl Into<Pat>) -> Pat {
    Pat::new(PatKind::Lzcount {
        operand: operand.into(),
    })
}
