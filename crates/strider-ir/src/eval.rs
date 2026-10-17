//! Integer evaluation at a declared width, in the `u128` carrier.
//!
//! Each function answers `None` where the operation is undefined or the width
//! has no `u128` modulus: a float, or an integer past 128 bits.

use crate::node::{IntBinaryOp, IntCmpOp, IntUnaryOp, ValueType};

/// The signed minimum representable in `ty`'s bit width, or `i128::MIN` at
/// 128 bits.
#[inline]
fn signed_min(ty: ValueType) -> i128 {
    let bits = ty.bit_width() as u32;
    if bits >= 128 {
        i128::MIN
    } else {
        -(1i128 << (bits - 1))
    }
}

/// Result masked to `ty`, or `None` when the operation is undefined (division
/// by zero, `INT_MIN / -1`) or `ty` has no `u128` modulus (a float, or past
/// 128 bits): every arm below wraps, masks and shifts at 128 bits.
///
/// Both operands are masked to `ty` at entry, the shift amount only after its
/// out-of-range test. Div, Rem and ShiftRight are not safe under
/// masking-commutativity, so they need the inputs already narrowed to give the
/// right answer on a caller that passed raw bits.
#[inline]
pub fn int_binary(op: IntBinaryOp, l: u128, r: u128, ty: ValueType) -> Option<u128> {
    let mask = ty.int_mask_u128()?;
    let bits = ty.bit_width() as u32;
    // Sleigh (opbehavior.cc:411) returns 0 when the shift amount reaches the
    // output width for IntLeft/IntRight, and `signbit ? calc_mask : 0` for
    // IntSright. Guarded at the DECLARED bit width, which matches Sleigh's
    // `8 * sizeout` at every type except `I1`, whose width is 1 while its
    // varnode is a byte; `get_signed_int` reads `I1` as 1-bit signed for the
    // same reason, so an `I1` shift or signed compare answers off strider's
    // boolean width, not Sleigh's. The lifter emits `I1` only from compares and
    // `Truncate(..):I1`, so no shift ever reaches it. Do NOT reduce the amount
    // modulo `bits`, nor mask it to `ty` before this test: either wraps an
    // out-of-range literal back into range, diverging from Sleigh by the full
    // shift output.
    let r_ge_bits = r >= u128::from(bits);
    let l = l & mask;
    let r = r & mask;
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

/// `None` for a float, or past the `u128` carrier, where the overflow arms
/// would detect at 128 bits rather than at `ty`'s declared width.
#[inline]
pub fn int_cmp(op: IntCmpOp, l: u128, r: u128, ty: ValueType) -> Option<bool> {
    // Unsigned arms compare raw u128s, so a narrow IntConst carrying high bits
    // beyond the type width would compare wrong without this mask. The signed
    // arms re-mask via get_signed_int, so the double-mask is idempotent there.
    let mask = ty.int_mask_u128()?;
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

/// `op` applied to `v` at `ty`'s width; `None` for a float or past 128 bits.
#[inline]
pub fn int_unary(op: IntUnaryOp, v: u128, ty: ValueType) -> Option<u128> {
    let raw = match op {
        IntUnaryOp::Neg => v.wrapping_neg(),
    };
    ty.get_unsigned_int(raw)
}

/// `v`, read signed at `in_ty`, at `out_ty`; `None` when either is a float or
/// past 128 bits.
#[inline]
pub fn sign_extend(v: u128, in_ty: ValueType, out_ty: ValueType) -> Option<u128> {
    let signed = in_ty.get_signed_int(v)? as u128;
    out_ty.get_unsigned_int(signed)
}

/// The set bits of `v` within `in_ty`'s width; `None` for a float or past 128
/// bits.
#[inline]
pub fn popcount(v: u128, in_ty: ValueType) -> Option<u128> {
    let masked = in_ty.get_unsigned_int(v)?;
    Some(u128::from(masked.count_ones()))
}

/// Leading-zero count within `in_ty`'s width, not the host's; `None` past 128
/// bits.
///
/// Counts over `8 * byte_size`, which is what `opbehavior.cc:791` does
/// (`count_leading_zeros(in1) - 8*(sizeof(uintb) - sizein)`). Only `I1`
/// separates that from `bit_width`.
#[inline]
pub fn lzcount(v: u128, in_ty: ValueType) -> Option<u128> {
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
mod tests {
    use super::*;

    /// The width bail these helpers rely on lives in `get_unsigned_int` /
    /// `get_signed_int`, not in the helpers themselves.
    #[test]
    fn width_past_the_u128_carrier_does_not_fold() {
        assert_eq!(lzcount(1, ValueType::int(256)), None);
        assert_eq!(popcount(1, ValueType::int(256)), None);
        assert_eq!(int_unary(IntUnaryOp::Neg, 1, ValueType::int(512)), None);
        assert_eq!(
            sign_extend(1, ValueType::int(256), ValueType::int(512)),
            None
        );
    }

    /// Sleigh's `OpBehaviorIntLeft` returns 0 once the amount reaches the
    /// output width, so the amount must not be masked back into range first.
    #[test]
    fn an_out_of_range_shift_amount_saturates_rather_than_wrapping() {
        assert_eq!(
            int_binary(IntBinaryOp::ShiftLeft, 1, 0x100, ValueType::int(8)),
            Some(0)
        );
    }

    #[test]
    fn unary_neg_masks_to_width() {
        assert_eq!(int_unary(IntUnaryOp::Neg, 1, ValueType::int(8)), Some(0xFF));
    }

    #[test]
    fn sign_extend_i8_to_i32() {
        assert_eq!(
            sign_extend(0x80, ValueType::int(8), ValueType::int(32)),
            Some(0xFFFF_FF80)
        );
    }

    #[test]
    fn popcount_masks_input_width() {
        assert_eq!(popcount(0x1FF, ValueType::int(8)), Some(8));
    }

    #[test]
    fn lzcount_zero_is_width_and_msb_is_zero() {
        assert_eq!(lzcount(0, ValueType::int(8)), Some(8));
        assert_eq!(lzcount(0x80, ValueType::int(8)), Some(0));
    }

    /// `expected == None` means the fold must skip.
    #[test]
    fn int_binary_masking_and_overflow_cases() {
        struct Case {
            case: &'static str,
            op: IntBinaryOp,
            l: u128,
            r: u128,
            ty: ValueType,
            expected: Option<u128>,
        }
        #[rustfmt::skip]
        let cases = [
            // INT_MIN / -1 overflows at every narrow signed width too, not just u64.
            Case { case: "sdiv_narrow_int_min_neg_one_skips (I32)", op: IntBinaryOp::Sdiv, l: 0x8000_0000, r: 0xFFFF_FFFF, ty: ValueType::int(32), expected: None },
            Case { case: "sdiv_narrow_int_min_neg_one_skips (I16)", op: IntBinaryOp::Sdiv, l: 0x8000, r: 0xFFFF, ty: ValueType::int(16), expected: None },
            Case { case: "sdiv_narrow_int_min_neg_one_skips (I8)", op: IntBinaryOp::Sdiv, l: 0x80, r: 0xFF, ty: ValueType::int(8), expected: None },
            Case { case: "srem_narrow_int_min_neg_one_skips (I32)", op: IntBinaryOp::Srem, l: 0x8000_0000, r: 0xFFFF_FFFF, ty: ValueType::int(32), expected: None },
            Case { case: "srem_narrow_int_min_neg_one_skips (I16)", op: IntBinaryOp::Srem, l: 0x8000, r: 0xFFFF, ty: ValueType::int(16), expected: None },
            Case { case: "srem_narrow_int_min_neg_one_skips (I8)", op: IntBinaryOp::Srem, l: 0x80, r: 0xFF, ty: ValueType::int(8), expected: None },
            // The rows below carry garbage bits above the type width in `l`, so an
            // unmasked eval gives a different (wrong) answer: 0x1FF / 2 = 0xFF, not
            // the correct 0xFF / 2 = 0x7F.
            Case { case: "int_binary_unsigned_div_unmasked_u8", op: IntBinaryOp::Div, l: 0x1FF, r: 2, ty: ValueType::int(8), expected: Some(0x7F) },
            // 7 is chosen to distinguish masked from raw: 0xFFFF % 7 = 1 but
            // 0x1FFFF % 7 = 3.
            Case { case: "int_binary_unsigned_rem_unmasked_u16", op: IntBinaryOp::Rem, l: 0x1FFFF, r: 7, ty: ValueType::int(16), expected: Some(1) },
            Case { case: "int_binary_unsigned_shr_unmasked_u8", op: IntBinaryOp::ShiftRight, l: 0x1FF, r: 1, ty: ValueType::int(8), expected: Some(0x7F) },
        ];
        for c in &cases {
            assert_eq!(int_binary(c.op, c.l, c.r, c.ty), c.expected, "{}", c.case);
        }
    }

    /// `I1` is the one type whose `bit_width` is not its byte size times 8, so the
    /// shift guard reads 1 where Sleigh's `8 * sizeout` reads 8, and
    /// `get_signed_int` reads the low bit as the sign.  Neither shape is producible
    /// by the lifter; the rows pin what the guard's comment names as its exception.
    #[test]
    fn int_shift_guard_at_i1_diverges_from_sleigh() {
        // Sleigh: `1 < 8` and `signbit_negative(1, 1 byte) == false`, so `1 >> 1`
        // is 0; strider takes the `r >= bits` branch over a negative `l`.
        assert_eq!(
            int_binary(IntBinaryOp::SShiftRight, 1, 1, ValueType::I1),
            Some(1),
        );
        // Sleigh reads both bytes as 1 and 0; strider reads the 1-bit `l` as -1.
        assert_eq!(int_cmp(IntCmpOp::Sless, 1, 0, ValueType::I1), Some(true));
    }

    /// Input-masking guards, all at I8.
    #[test]
    fn int_cmp_masking_cases() {
        struct Case {
            case: &'static str,
            op: IntCmpOp,
            l: u128,
            r: u128,
            expected: bool,
            why: &'static str,
        }
        #[rustfmt::skip]
        let cases = [
            // Every row's `l` carries bits above I8, so an unmasked eval flips the
            // expected verdict.
            Case { case: "int_cmp_equal_unmasked_u8", op: IntCmpOp::Equal, l: 0x1FF, r: 0xFF, expected: true, why: "Equal must mask both sides to I8 before comparing" },
            Case { case: "int_cmp_less_unmasked_u8", op: IntCmpOp::Less, l: 0x100, r: 0x01, expected: true, why: "Less must mask both sides to I8 before comparing" },
            Case { case: "int_cmp_carry_unmasked_u8", op: IntCmpOp::Carry, l: 0x100, r: 0, expected: false, why: "Carry must mask both sides before checking overflow" },
        ];
        for c in &cases {
            assert_eq!(
                int_cmp(c.op, c.l, c.r, ValueType::int(8)).unwrap(),
                c.expected,
                "{}: {}",
                c.case,
                c.why,
            );
        }
    }

    /// The `bits >= 128` arms, which use wrapping add/sub plus sign-bit logic
    /// rather than the narrow range check.
    #[test]
    fn int_cmp_i128_overflow_arms_cases() {
        struct Case {
            case: &'static str,
            op: IntCmpOp,
            l: u128,
            r: u128,
            expected: bool,
            why: &'static str,
        }
        #[rustfmt::skip]
        let cases = [
            Case { case: "scarry_i128_max_plus_one", op: IntCmpOp::Scarry, l: i128::MAX as u128, r: 1, expected: true, why: "i128::MAX + 1 signed-overflows" },
            Case { case: "sborrow_i128_min_minus_one", op: IntCmpOp::Sborrow, l: i128::MIN as u128, r: 1, expected: true, why: "i128::MIN - 1 signed-overflows" },
            Case { case: "carry_u128_max_plus_one", op: IntCmpOp::Carry, l: u128::MAX, r: 1, expected: true, why: "u128::MAX + 1 unsigned-overflows (sum wraps below l)" },
            // Controls: no overflow on the same arms.
            Case { case: "scarry_i128_zero_no_overflow", op: IntCmpOp::Scarry, l: 0, r: 1, expected: false, why: "0 + 1 does not signed-overflow at I128" },
            Case { case: "sborrow_i128_zero_no_overflow", op: IntCmpOp::Sborrow, l: 0, r: 1, expected: false, why: "0 - 1 = -1 does not signed-overflow at I128 (both operands non-negative)" },
            Case { case: "carry_i128_zero_no_overflow", op: IntCmpOp::Carry, l: 0, r: 1, expected: false, why: "0 + 1 does not unsigned-overflow at I128" },
        ];
        for c in &cases {
            assert_eq!(
                int_cmp(c.op, c.l, c.r, ValueType::int(128)).unwrap(),
                c.expected,
                "{}: {}",
                c.case,
                c.why,
            );
        }
    }

    /// The value-producing signed arms.
    #[test]
    fn int_binary_signed_value_folds_cases() {
        struct Case {
            case: &'static str,
            op: IntBinaryOp,
            l: u128,
            r: u128,
            ty: ValueType,
            expected: Option<u128>,
        }
        #[rustfmt::skip]
        let cases = [
            // 2, truncated toward zero.
            Case { case: "sdiv_neg_by_neg_i8", op: IntBinaryOp::Sdiv, l: 0xFB, r: 0xFE, ty: ValueType::int(8), expected: Some(2) },
            // The remainder takes the dividend's sign, then masks back to I8.
            Case { case: "srem_neg_by_pos_i8", op: IntBinaryOp::Srem, l: 0xF9, r: 2, ty: ValueType::int(8), expected: Some(0xFF) },
            // Sign fill, not the logical zero-fill that would give 0x40.
            Case { case: "sshr_neg_one_i8", op: IntBinaryOp::SShiftRight, l: 0x80, r: 1, ty: ValueType::int(8), expected: Some(0xC0) },
        ];
        for c in &cases {
            assert_eq!(int_binary(c.op, c.l, c.r, c.ty), c.expected, "{}", c.case);
        }
    }

    // Shifting by the full bit width or beyond zeroes the value in Sleigh
    // (opbehavior.cc:411), and sign-fills for INT_SRIGHT. Reducing the amount
    // modulo the width instead diverges by a factor of 2^bits.

    /// Out-of-range (>= bit-width) shift semantics; each `why` names the Sleigh
    /// rule the row pins.
    #[test]
    fn int_binary_out_of_range_shift_cases() {
        struct Case {
            case: &'static str,
            op: IntBinaryOp,
            l: u128,
            r: u128,
            ty: ValueType,
            expected: Option<u128>,
            why: &'static str,
        }
        #[rustfmt::skip]
        let cases = [
            Case { case: "int_binary_shl_at_bit_width_returns_zero_u32", op: IntBinaryOp::ShiftLeft, l: 1, r: 32, ty: ValueType::int(32), expected: Some(0),
                   why: "Sleigh: 1u32 << 32 = 0 (`r >= 8*sizeout` returns 0 per opbehavior.cc:411). A modulo-reduced amount computes `1 << (32 % 32) = 1 << 0 = 1`, diverging from Sleigh." },
            Case { case: "int_binary_shl_at_bit_width_returns_zero_u64", op: IntBinaryOp::ShiftLeft, l: 1, r: 64, ty: ValueType::int(64), expected: Some(0),
                   why: "Sleigh: 1u64 << 64 = 0.  A modulo-reduced amount computes `1 << (64 % 64) = 1`." },
            Case { case: "int_binary_shl_above_bit_width_returns_zero_u32", op: IntBinaryOp::ShiftLeft, l: 0xFF, r: 40, ty: ValueType::int(32), expected: Some(0),
                   why: "Sleigh: shift > bit-width still returns 0.  A modulo-reduced amount computes `0xFF << (40 % 32) = 0xFF << 8 = 0xFF00`." },
            Case { case: "int_binary_shr_at_bit_width_returns_zero_u32", op: IntBinaryOp::ShiftRight, l: 0xFFFF_FFFF, r: 32, ty: ValueType::int(32), expected: Some(0),
                   why: "Sleigh: 0xFFFFFFFFu32 >> 32 = 0 per opbehavior.cc:432.  A modulo-reduced amount computes `0xFFFFFFFF >> (32 % 32) = 0xFFFFFFFF`." },
            Case { case: "int_binary_sshr_at_bit_width_negative_returns_all_ones_u32", op: IntBinaryOp::SShiftRight, l: 0xFFFF_FFFF, r: 32, ty: ValueType::int(32), expected: Some(0xFFFF_FFFF),
                   why: "Sleigh: signed-negative >> bit-width fills with the sign bit (= 0xFFFFFFFF) per opbehavior.cc:454-460." },
            Case { case: "int_binary_sshr_at_bit_width_positive_returns_zero_u32", op: IntBinaryOp::SShiftRight, l: 0x7FFF_FFFF, r: 32, ty: ValueType::int(32), expected: Some(0),
                   why: "Sleigh: signed-non-negative >> bit-width = 0 (no sign bit to fill)." },
        ];
        for c in &cases {
            assert_eq!(
                int_binary(c.op, c.l, c.r, c.ty),
                c.expected,
                "{}: {}",
                c.case,
                c.why,
            );
        }
    }
}
