use strider_ir::{IntBinaryOp, IntCmpOp, IntUnaryOp, node::ValueType};

use anyhow::anyhow;

use crate::error::Result;

/// The signed minimum and maximum representable in `ty`'s bit width
/// (`-2^(bits-1)` .. `2^(bits-1) - 1`, or `i128::MIN`/`MAX` at ≥128 bits).
fn signed_min_max(ty: ValueType) -> (i128, i128) {
    let bits = ty.bit_width() as u32;
    if bits >= 128 {
        (i128::MIN, i128::MAX)
    } else {
        let min = -(1i128 << (bits - 1));
        let max = (1i128 << (bits - 1)) - 1;
        (min, max)
    }
}

/// Sign-extends `v` to `i128` per `ty`'s bit width, erroring if `ty` is not an
/// integer (the shared "expected integer type" message both the comparison
/// evaluator and the `SignExtend` const-fold rule emit).
pub(crate) fn require_signed(ty: ValueType, v: u128) -> Result<i128> {
    ty.get_signed_int(v)
        .ok_or_else(|| anyhow!("expected integer type, got {ty:?}"))
}

// ── integer constant evaluation ───────────────────────────────────────────────

/// Evaluates `op(l, r)` as an integer arithmetic operation, returning the
/// result masked to `ty`, or `None` if the operation is undefined (e.g.
/// division by zero).
///
/// Both `l` and `r` are masked to `ty.bit_mask_u128()` at entry.
/// IntConst values are normally already masked by `build_int_const`, but
/// re-masking is cheap insurance against any caller passing raw bits.
/// Operations that aren't safe under masking-commutativity (Div, Rem,
/// ShiftRight, signed cmps) need this mask to give correct results.
pub(crate) fn eval_int_binary(op: IntBinaryOp, l: u128, r: u128, ty: ValueType) -> Option<u128> {
    let mask = ty.bit_mask_u128();
    let l = l & mask;
    let r = r & mask;
    let bits = ty.bit_width() as u32;
    // Sleigh's `OpBehaviorIntLeft::evaluateBinary` (sleigh/src/opbehavior.cc:411)
    // returns 0 when the shift amount is `>= 8 * sizeout`.  `IntRight` matches.
    // `IntSright` returns `signbit ? calc_mask : 0`.  Mirroring this here keeps
    // the constant-fold's evaluation consistent with Sleigh's runtime semantics
    // — pre-fix the evaluator computed `r % bits` and diverged from Sleigh
    // by the full shift output for any literal `r >= bits`.
    let r_ge_bits = r >= u128::from(bits);
    // Shift arms below only call `shift` inside the `!r_ge_bits` branch,
    // so `s < bits <= u128::from(u32::MAX)` and the truncation is lossless.
    #[allow(clippy::cast_possible_truncation)]
    let shift = |s: u128| -> u32 { s as u32 };
    let raw: u128 = match op {
        IntBinaryOp::Add => l.wrapping_add(r),
        IntBinaryOp::Mul => l.wrapping_mul(r),
        IntBinaryOp::And => l & r,
        IntBinaryOp::Or => l | r,
        IntBinaryOp::Xor => l ^ r,
        IntBinaryOp::ShiftLeft => {
            if r_ge_bits {
                0
            } else {
                l.wrapping_shl(shift(r)) & mask
            }
        }
        IntBinaryOp::ShiftRight => {
            if r_ge_bits {
                0
            } else {
                l.wrapping_shr(shift(r))
            }
        }
        IntBinaryOp::SShiftRight => {
            let sl = ty.get_signed_int(l)?;
            if r_ge_bits {
                // Sign-bit-set → fill with all-ones; sign-bit-clear → zero.
                if sl < 0 { mask } else { 0 }
            } else {
                sl.wrapping_shr(shift(r)) as u128 & mask
            }
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
            // integer width.  At narrow widths the i128 division "looks
            // well-defined" (e.g. -i32::MIN as i128 = 2^31 fits), but the
            // mask-back to ty would silently wrap to INT_MIN — not the
            // mathematical result.  Skip rather than emit a wraparound.
            let (int_min, _) = signed_min_max(ty);
            if sl == int_min && sr == -1 {
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
            let sl = ty.get_signed_int(l)?;
            let sr = ty.get_signed_int(r)?;
            if sr == 0 {
                return None;
            }
            // Signed-overflow guard: INT_MIN % -1 is mathematically 0 but
            // hardware idiv raises #DE; treat it as undefined and skip,
            // matching the Sdiv case.
            let (int_min, _) = signed_min_max(ty);
            if sl == int_min && sr == -1 {
                return None;
            }
            sl.wrapping_rem(sr) as u128 & mask
        }
    };
    Some(raw & mask)
}

/// Evaluates a comparison on two constant integer values.
pub(crate) fn eval_int_cmp(op: IntCmpOp, l: u128, r: u128, ty: ValueType) -> Result<bool> {
    // Mask both inputs to ty at entry.  Unsigned comparisons (Equal, Less,
    // LessEqual, Carry) operate on raw u128s and would otherwise return
    // wrong answers for narrow IntConsts that carry high bits beyond the
    // type width.  The signed arms re-mask via get_signed_int so the
    // double-mask is idempotent for them.
    let mask = ty.bit_mask_u128();
    let l = l & mask;
    let r = r & mask;

    let signed = |v: u128| -> Result<i128> { require_signed(ty, v) };
    let bits = ty.bit_width() as u32;
    // Carry / signed-overflow comparisons: shifting both operands to the TOP of
    // the host width turns the type's width-`bits` overflow into host-width
    // overflow, so stdlib's overflow flag is one SSoT across every width.
    // `top == 0` at bits >= 128 reduces to a plain i128/u128 overflowing op
    // (wider-than-128 types fold at 128 bits, like the rest of this module's
    // u128-domain evaluation).
    let top = 128u32.saturating_sub(bits);

    Ok(match op {
        IntCmpOp::Equal => l == r,
        IntCmpOp::Less => l < r,
        IntCmpOp::Sless => signed(l)? < signed(r)?,
        // Unsigned add overflow at the type width.
        IntCmpOp::Carry => (l << top).overflowing_add(r << top).1,
        // Signed add overflow at the type width.
        IntCmpOp::Scarry => (signed(l)? << top).overflowing_add(signed(r)? << top).1,
        // Signed sub overflow at the type width.
        IntCmpOp::Sborrow => (signed(l)? << top).overflowing_sub(signed(r)? << top).1,
    })
}

/// Evaluates a unary integer op on a constant, masked to `ty`.
pub(crate) fn eval_int_unary(op: IntUnaryOp, v: u128, ty: ValueType) -> Option<u128> {
    let raw = match op {
        IntUnaryOp::Neg => v.wrapping_neg(),
    };
    ty.get_unsigned_int(raw)
}

/// Sign-extends `v` from `in_ty`, masked to `out_ty`.
pub(crate) fn eval_sign_extend(v: u128, in_ty: ValueType, out_ty: ValueType) -> Option<u128> {
    let signed = require_signed(in_ty, v).ok()? as u128;
    out_ty.get_unsigned_int(signed)
}

/// Population count of `v` masked to `in_ty`.
pub(crate) fn eval_popcount(v: u128, in_ty: ValueType) -> Option<u128> {
    let masked = in_ty.get_unsigned_int(v)?;
    Some(u128::from(masked.count_ones()))
}

/// Leading-zero count of `v` within `in_ty`'s width; `None` for widths > 128.
pub(crate) fn eval_lzcount(v: u128, in_ty: ValueType) -> Option<u128> {
    let masked = in_ty.get_unsigned_int(v)?;
    let bits = in_ty.bit_width() as u32;
    if bits > 128 {
        return None;
    }
    Some(if masked == 0 {
        u128::from(bits)
    } else if bits == 128 {
        u128::from(masked.leading_zeros())
    } else {
        u128::from((masked << (128 - bits)).leading_zeros())
    })
}

#[cfg(test)]
mod eval_helper_tests {
    use super::*;
    use strider_ir::node::ValueType;

    #[test]
    fn unary_neg_masks_to_width() {
        assert_eq!(
            eval_int_unary(IntUnaryOp::Neg, 1, ValueType::I8),
            Some(0xFF)
        );
    }

    #[test]
    fn sign_extend_i8_to_i32() {
        assert_eq!(
            eval_sign_extend(0x80, ValueType::I8, ValueType::I32),
            Some(0xFFFF_FF80)
        );
    }

    #[test]
    fn popcount_masks_input_width() {
        assert_eq!(eval_popcount(0x1FF, ValueType::I8), Some(8));
    }

    #[test]
    fn lzcount_zero_is_width_and_msb_is_zero() {
        assert_eq!(eval_lzcount(0, ValueType::I8), Some(8));
        assert_eq!(eval_lzcount(0x80, ValueType::I8), Some(0));
    }
}
