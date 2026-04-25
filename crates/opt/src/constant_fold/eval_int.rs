use ir::node::NodeOutputType;
use ir::{IntBinaryOp, IntCmpOp};

use crate::error::{ErrorKind, Result};

// ── integer constant evaluation ───────────────────────────────────────────────

/// Evaluates `op(l, r)` as an integer arithmetic operation, returning the
/// result masked to `ty`, or `None` if the operation is undefined (e.g.
/// division by zero).
pub(super) fn eval_int_binary(
    op: IntBinaryOp,
    l: u128,
    r: u128,
    ty: NodeOutputType,
) -> Option<u128> {
    let mask = ty.bit_mask_u128();
    let l = l & mask;
    let r = r & mask;
    let bits = ty.bit_width() as u32;
    // Shift amounts are masked to prevent UB.
    let shift = |s: u128| -> u32 {
        if bits == 0 { 0 } else { (s as u32) % bits }
    };
    let raw: u128 = match op {
        IntBinaryOp::Add => l.wrapping_add(r),
        IntBinaryOp::Sub => l.wrapping_sub(r),
        IntBinaryOp::Mul => l.wrapping_mul(r),
        IntBinaryOp::And => l & r,
        IntBinaryOp::Or => l | r,
        IntBinaryOp::Xor => l ^ r,
        IntBinaryOp::ShiftLeft => l.wrapping_shl(shift(r)) & mask,
        IntBinaryOp::ShiftRight => l.wrapping_shr(shift(r)),
        IntBinaryOp::SShiftRight => {
            let sl = ty.get_signed_int_i128(l)?;
            sl.wrapping_shr(shift(r)) as u128 & mask
        }
        IntBinaryOp::Div => {
            if r == 0 {
                return None;
            }
            l / r
        }
        IntBinaryOp::Sdiv => {
            let sl = ty.get_signed_int_i128(l)?;
            let sr = ty.get_signed_int_i128(r)?;
            if sr == 0 {
                return None;
            }
            // Two's-complement overflow: i128::MIN / -1 is undefined in Rust.
            if sl == i128::MIN && sr == -1 {
                return None;
            }
            sl.wrapping_div(sr) as u128 & mask
        }
        IntBinaryOp::Rem => {
            if r == 0 {
                return None;
            }
            l % r
        }
        IntBinaryOp::Srem => {
            let sl = ty.get_signed_int_i128(l)?;
            let sr = ty.get_signed_int_i128(r)?;
            if sr == 0 {
                return None;
            }
            if sl == i128::MIN && sr == -1 {
                return None;
            }
            sl.wrapping_rem(sr) as u128 & mask
        }
    };
    Some(raw & mask)
}

/// Evaluates a comparison on two constant integer values.
pub(super) fn eval_int_cmp(op: IntCmpOp, l: u128, r: u128, ty: NodeOutputType) -> Result<bool> {
    Ok(match op {
        IntCmpOp::Equal => l == r,
        IntCmpOp::Less => l < r,
        IntCmpOp::LessEqual => l <= r,
        IntCmpOp::Sless => {
            ty.get_signed_int_i128(l)
                .ok_or(ErrorKind::ExpectedIntegerType(ty))?
                < ty.get_signed_int_i128(r)
                    .ok_or(ErrorKind::ExpectedIntegerType(ty))?
        }
        IntCmpOp::SlessEqual => {
            ty.get_signed_int_i128(l)
                .ok_or(ErrorKind::ExpectedIntegerType(ty))?
                <= ty
                    .get_signed_int_i128(r)
                    .ok_or(ErrorKind::ExpectedIntegerType(ty))?
        }
        IntCmpOp::Carry => {
            // Carry = unsigned addition overflows the type.
            let max = ty
                .get_unsigned_int_u128(u128::MAX)
                .ok_or(ErrorKind::ExpectedIntegerType(ty))?;
            // Work at u256-equivalent precision: use u128 wrapping and check
            // for overflow separately. For widths ≤ 64 the old u128 trick works;
            // for U128 we detect overflow by checking if the sum wraps.
            let sum = l.wrapping_add(r);
            // Carry iff sum < either operand (wrapping overflow).
            sum < l || (max < u128::MAX && l.wrapping_add(r) > max)
        }
        IntCmpOp::Borrow => {
            // Borrow = l < r (unsigned subtraction borrows).
            l < r
        }
        IntCmpOp::Scarry => {
            // Signed overflow of l + r.
            let sl = ty
                .get_signed_int_i128(l)
                .ok_or(ErrorKind::ExpectedIntegerType(ty))?;
            let sr = ty
                .get_signed_int_i128(r)
                .ok_or(ErrorKind::ExpectedIntegerType(ty))?;
            let bits = ty.bit_width() as u32;
            // Use i128 arithmetic; for U128 this is exact.
            let result = sl.wrapping_add(sr);
            if bits >= 128 {
                // At U128: detect signed overflow by checking sign bits
                // (same sign inputs but different sign output).
                let sign_l = sl < 0;
                let sign_r = sr < 0;
                let sign_res = result < 0;
                sign_l == sign_r && sign_l != sign_res
            } else {
                let min_val = -(1i128 << (bits - 1));
                let max_val = (1i128 << (bits - 1)) - 1;
                result < min_val || result > max_val
            }
        }
        IntCmpOp::Sborrow => {
            // Signed overflow of l - r.
            let sl = ty
                .get_signed_int_i128(l)
                .ok_or(ErrorKind::ExpectedIntegerType(ty))?;
            let sr = ty
                .get_signed_int_i128(r)
                .ok_or(ErrorKind::ExpectedIntegerType(ty))?;
            let bits = ty.bit_width() as u32;
            let result = sl.wrapping_sub(sr);
            if bits >= 128 {
                let sign_l = sl < 0;
                let sign_r = sr < 0;
                let sign_res = result < 0;
                sign_l != sign_r && sign_l != sign_res
            } else {
                let min_val = -(1i128 << (bits - 1));
                let max_val = (1i128 << (bits - 1)) - 1;
                result < min_val || result > max_val
            }
        }
    })
}
