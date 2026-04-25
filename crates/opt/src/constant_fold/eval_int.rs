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
    // Defensive: IntConst(u64) values are not guaranteed to be masked to the
    // declared type's width — `make_int_const` stores the raw u64, and the
    // analyzer's vn_io lifter feeds raw Sleigh `VnAddr.off` values through.
    // Operations safe under masking-commutativity (Add, Sub, Mul, And, Or,
    // Xor, ShiftLeft) would still produce the right answer because the final
    // `ty.get_unsigned_int(raw)` cancels any high bits, but Div, Rem, and
    // ShiftRight are NOT commutative with masking and would give wrong
    // results. Mask once at entry; the `?` skips evaluation entirely for
    // U128/U256 (consistent with the existing per-arm fallthroughs).
    let l = ty.get_unsigned_int(l)?;
    let r = ty.get_unsigned_int(r)?;
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
            // Signed overflow: INT_MIN / -1 is undefined for every signed
            // integer width. The narrow-type case looks "well-defined" at
            // i64 width (e.g. -i32::MIN as i64 = 2^31 fits), but masking
            // back to the type silently wraps to INT_MIN, which is not the
            // mathematical result. Skip rather than emit a wraparound.
            let bits = ty.bit_width() as u32;
            let int_min: i64 = i64::MIN >> (64 - bits);
            if sl == int_min && sr == -1 {
                return None;
            }
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
            // Signed-overflow guard: INT_MIN % -1 is mathematically 0 but
            // hardware idiv raises #DE; treat it as undefined and skip,
            // matching the Sdiv case.
            let bits = ty.bit_width() as u32;
            let int_min: i64 = i64::MIN >> (64 - bits);
            if sl == int_min && sr == -1 {
                return None;
            }
            (sl % sr) as u64
        }
    };
    ty.get_unsigned_int(raw)
}

/// Evaluates a comparison on two constant integer values.
//
// `IntCmpOp::Less` and `IntCmpOp::Borrow` both evaluate to `l < r` because an
// unsigned subtract borrows iff the minuend is less than the subtrahend. The
// two operations are conceptually distinct — keep them as separate arms with
// their own names rather than merging them into `Less | Borrow => l < r`.
#[allow(clippy::match_same_arms)]
pub(super) fn eval_int_cmp(op: IntCmpOp, l: u64, r: u64, ty: NodeOutputType) -> Result<bool> {
    // See `eval_int_binary` — mask both inputs to `ty` at entry. The
    // unsigned comparisons (Equal, Less, LessEqual, Carry, Borrow) operate
    // on raw u64s and would otherwise return wrong answers for U8/U16/U32
    // IntConsts that carry high bits beyond the type width. The signed
    // arms (`Sless`, `Scarry`, …) re-mask via `get_signed_int` so the
    // double-mask is idempotent for them.
    let l = ty
        .get_unsigned_int(l)
        .ok_or_else(|| ErrorKind::ExpectedIntegerType(ty))?;
    let r = ty
        .get_unsigned_int(r)
        .ok_or_else(|| ErrorKind::ExpectedIntegerType(ty))?;

    let signed = |v: u64| -> Result<i64> {
        ty.get_signed_int(v)
            .ok_or_else(|| ErrorKind::ExpectedIntegerType(ty).into())
    };
    let unsigned_max = || -> Result<u64> {
        ty.get_unsigned_int(u64::MAX)
            .ok_or_else(|| ErrorKind::ExpectedIntegerType(ty).into())
    };
    let bits = ty.bit_width() as u32;
    let signed_min_max = || -> (i128, i128) {
        let min = -(1i128 << (bits - 1));
        let max = (1i128 << (bits - 1)) - 1;
        (min, max)
    };

    Ok(match op {
        IntCmpOp::Equal => l == r,
        IntCmpOp::Less => l < r,
        IntCmpOp::LessEqual => l <= r,
        IntCmpOp::Sless => signed(l)? < signed(r)?,
        IntCmpOp::SlessEqual => signed(l)? <= signed(r)?,
        IntCmpOp::Carry => {
            // Unsigned add overflow: l + r > type's max unsigned value.
            (l as u128 + r as u128) > unsigned_max()? as u128
        }
        IntCmpOp::Borrow => l < r,
        IntCmpOp::Scarry => {
            let (min, max) = signed_min_max();
            let result = signed(l)? as i128 + signed(r)? as i128;
            result < min || result > max
        }
        IntCmpOp::Sborrow => {
            let (min, max) = signed_min_max();
            let result = signed(l)? as i128 - signed(r)? as i128;
            result < min || result > max
        }
    })
}
