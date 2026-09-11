use strider_ir::node::ValueType;
use strider_ir::{IntBinaryOp, IntCmpOp, IntUnaryOp};

/// The signed minimum representable in `ty`'s bit width, or `i128::MIN` at
/// 128 bits.
fn signed_min(ty: ValueType) -> i128 {
    let bits = ty.bit_width() as u32;
    if bits >= 128 {
        i128::MIN
    } else {
        -(1i128 << (bits - 1))
    }
}

/// Result masked to `ty`, or `None` when the operation is undefined (division
/// by zero, `INT_MIN / -1`) or `ty` is past the `u128` carrier: every arm below
/// wraps, masks and shifts at 128 bits, which is the wrong modulus for a wider
/// declared width.
///
/// Both operands are masked to `ty` at entry. Div, Rem and ShiftRight are not
/// safe under masking-commutativity, so they need the inputs already narrowed
/// to give the right answer on a caller that passed raw bits.
pub(crate) fn eval_int_binary(op: IntBinaryOp, l: u128, r: u128, ty: ValueType) -> Option<u128> {
    if ty.bit_width() > 128 {
        return None;
    }
    let mask = ty.bit_mask_u128();
    let l = l & mask;
    let r = r & mask;
    let bits = ty.bit_width() as u32;
    // Sleigh (opbehavior.cc:411) returns 0 when the shift amount reaches the
    // output width for IntLeft/IntRight, and `signbit ? calc_mask : 0` for
    // IntSright. Guarded at the DECLARED bit width, which matches Sleigh's
    // `8 * sizeout` at every type except `I1`, whose width is 1 while its
    // varnode is a byte; `get_signed_int` reads `I1` as 1-bit signed for the
    // same reason, so an `I1` shift or signed compare answers off strider's
    // boolean width, not Sleigh's. The lifter emits `I1` only from compares and
    // `Truncate(..):I1`, so no shift ever reaches it. Do NOT reduce the amount
    // modulo `bits`: that diverges from Sleigh by the full shift output for any
    // literal `r >= bits`.
    let r_ge_bits = r >= u128::from(bits);
    // `shift` is only reached inside the `!r_ge_bits` branch, so `s < bits` and
    // the truncation is lossless.
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
            // INT_MIN / -1 is undefined at every signed width. At narrow widths
            // the i128 division looks well-defined (2^31 fits), but masking back
            // to `ty` wraps it to INT_MIN, not the mathematical result.
            let int_min = signed_min(ty);
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
            // INT_MIN % -1 is mathematically 0, but hardware idiv raises #DE.
            // Treat it as undefined, matching the Sdiv arm.
            let int_min = signed_min(ty);
            if sl == int_min && sr == -1 {
                return None;
            }
            sl.wrapping_rem(sr) as u128 & mask
        }
    };
    Some(raw & mask)
}

/// `None` past the `u128` carrier, where the overflow arms would detect at 128
/// bits rather than at `ty`'s declared width.
pub(crate) fn eval_int_cmp(op: IntCmpOp, l: u128, r: u128, ty: ValueType) -> Option<bool> {
    if ty.bit_width() > 128 {
        return None;
    }
    // Unsigned arms compare raw u128s, so a narrow IntConst carrying high bits
    // beyond the type width would compare wrong without this mask. The signed
    // arms re-mask via get_signed_int, so the double-mask is idempotent there.
    let mask = ty.bit_mask_u128();
    let l = l & mask;
    let r = r & mask;

    let signed = |v: u128| ty.get_signed_int(v);
    let bits = ty.bit_width() as u32;
    // Shifting both operands to the top of the host width turns width-`bits`
    // overflow into host-width overflow, so stdlib's overflow flag works at
    // every width. `top == 0` at 128 bits degrades to a plain i128/u128
    // overflowing op.
    let top = 128u32.saturating_sub(bits);

    Some(match op {
        IntCmpOp::Equal => l == r,
        IntCmpOp::Less => l < r,
        IntCmpOp::Sless => signed(l)? < signed(r)?,
        IntCmpOp::Carry => (l << top).overflowing_add(r << top).1,
        IntCmpOp::Scarry => (signed(l)? << top).overflowing_add(signed(r)? << top).1,
        IntCmpOp::Sborrow => (signed(l)? << top).overflowing_sub(signed(r)? << top).1,
    })
}

/// The unary/extend/count helpers below are width-safe only because
/// `get_unsigned_int` / `get_signed_int` return `None` past 128 bits; masking
/// with `bit_mask_u128` instead would silently truncate an I256/I512.
pub(crate) fn eval_int_unary(op: IntUnaryOp, v: u128, ty: ValueType) -> Option<u128> {
    let raw = match op {
        IntUnaryOp::Neg => v.wrapping_neg(),
    };
    ty.get_unsigned_int(raw)
}

pub(crate) fn eval_sign_extend(v: u128, in_ty: ValueType, out_ty: ValueType) -> Option<u128> {
    let signed = in_ty.get_signed_int(v)? as u128;
    out_ty.get_unsigned_int(signed)
}

pub(crate) fn eval_popcount(v: u128, in_ty: ValueType) -> Option<u128> {
    let masked = in_ty.get_unsigned_int(v)?;
    Some(u128::from(masked.count_ones()))
}

/// Leading-zero count within `in_ty`'s width, not the host's; `None` past 128
/// bits.
///
/// Counts over `8 * byte_size`, which is what `opbehavior.cc:791` does
/// (`count_leading_zeros(in1) - 8*(sizeof(uintb) - sizein)`). Only `I1`
/// separates that from `bit_width`.
pub(crate) fn eval_lzcount(v: u128, in_ty: ValueType) -> Option<u128> {
    let masked = in_ty.get_unsigned_int(v)?;
    let bits = (in_ty.byte_size() * 8) as u32;
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

    /// The width bail these helpers rely on lives in `get_unsigned_int` /
    /// `get_signed_int`, not in the helpers themselves.
    #[test]
    fn width_past_the_u128_carrier_does_not_fold() {
        assert_eq!(eval_lzcount(1, ValueType::I256), None);
        assert_eq!(eval_popcount(1, ValueType::I256), None);
        assert_eq!(eval_int_unary(IntUnaryOp::Neg, 1, ValueType::I512), None);
        assert_eq!(eval_sign_extend(1, ValueType::I256, ValueType::I512), None);
    }

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
