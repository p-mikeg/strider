use ir::node::NodeOutputType;
use ir::{IntBinaryOp, IntCmpOp};

use crate::error::{ErrorKind, Result};

// ── integer constant evaluation ───────────────────────────────────────────────

/// Evaluates `op(l, r)` as an integer arithmetic operation, returning the
/// result masked to `ty`, or `None` if the operation is undefined (e.g.
/// division by zero).
pub(super) fn eval_int_binary(
    op: IntBinaryOp,
    l: u64,
    r: u64,
    ty: NodeOutputType,
) -> Option<u64> {
    let bits = ty.bit_width() as u64;
    // Shift amounts are masked to prevent UB; u32 is required by wrapping_shl/shr.
    let shift = |s: u64| -> u32 { (s & (bits - 1)) as u32 };
    let raw: u64 = match op {
        IntBinaryOp::Add => l.wrapping_add(r),
        IntBinaryOp::Sub => l.wrapping_sub(r),
        IntBinaryOp::Mul => l.wrapping_mul(r),
        IntBinaryOp::And => l & r,
        IntBinaryOp::Or => l | r,
        IntBinaryOp::Xor => l ^ r,
        IntBinaryOp::ShiftLeft => l.wrapping_shl(shift(r)),
        IntBinaryOp::ShiftRight => l.wrapping_shr(shift(r)),
        IntBinaryOp::SShiftRight => {
            let sl = ty.get_signed_int(l)?;
            (sl >> shift(r)) as u64
        }
        IntBinaryOp::Div => {
            if r == 0 {
                return None;
            }
            l / r
        }
        IntBinaryOp::Sdiv => {
            let sl = ty.get_signed_int(l)?;
            let sr = ty.get_signed_int(r)?;
            if sr == 0 {
                return None;
            }
            if sl == i64::MIN && sr == -1 {
                return None;
            } // overflow
            (sl / sr) as u64
        }
        IntBinaryOp::Rem => {
            if r == 0 {
                return None;
            }
            l % r
        }
        IntBinaryOp::Srem => {
            let sl = ty.get_signed_int(l)?;
            let sr = ty.get_signed_int(r)?;
            if sr == 0 {
                return None;
            }
            (sl % sr) as u64
        }
    };
    ty.get_unsigned_int(raw)
}

/// Evaluates a comparison on two constant integer values.
pub(super) fn eval_int_cmp(op: IntCmpOp, l: u64, r: u64, ty: NodeOutputType) -> Result<bool> {
    Ok(match op {
        IntCmpOp::Equal => l == r,
        IntCmpOp::Less => l < r,
        IntCmpOp::LessEqual => l <= r,
        IntCmpOp::Sless => {
            ty.get_signed_int(l)
                .ok_or(ErrorKind::ExpectedIntegerType(ty))?
                < ty.get_signed_int(r)
                    .ok_or(ErrorKind::ExpectedIntegerType(ty))?
        }
        IntCmpOp::SlessEqual => {
            ty.get_signed_int(l)
                .ok_or(ErrorKind::ExpectedIntegerType(ty))?
                <= ty
                    .get_signed_int(r)
                    .ok_or(ErrorKind::ExpectedIntegerType(ty))?
        }
        IntCmpOp::Carry => {
            // Carry = unsigned addition overflows the type.
            let max = ty
                .get_unsigned_int(u64::MAX)
                .ok_or(ErrorKind::ExpectedIntegerType(ty))? as u128;
            (l as u128 + r as u128) > max
        }
        IntCmpOp::Borrow => {
            // Borrow = l < r (unsigned subtraction borrows).
            l < r
        }
        IntCmpOp::Scarry => {
            // Signed overflow of l + r.
            let sl = ty
                .get_signed_int(l)
                .ok_or(ErrorKind::ExpectedIntegerType(ty))? as i128;
            let sr = ty
                .get_signed_int(r)
                .ok_or(ErrorKind::ExpectedIntegerType(ty))? as i128;
            let result = sl + sr;
            let bits = ty.bit_width() as u32;
            let min_val = -(1i128 << (bits - 1));
            let max_val = (1i128 << (bits - 1)) - 1;
            result < min_val || result > max_val
        }
        IntCmpOp::Sborrow => {
            // Signed overflow of l - r.
            let sl = ty
                .get_signed_int(l)
                .ok_or(ErrorKind::ExpectedIntegerType(ty))? as i128;
            let sr = ty
                .get_signed_int(r)
                .ok_or(ErrorKind::ExpectedIntegerType(ty))? as i128;
            let result = sl - sr;
            let bits = ty.bit_width() as u32;
            let min_val = -(1i128 << (bits - 1));
            let max_val = (1i128 << (bits - 1)) - 1;
            result < min_val || result > max_val
        }
    })
}
