/// Integer variants name their width in bits. There is no separate boolean
/// category: `I1` is an ordinary integer type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ValueType {
    /// The boolean type: a comparison or logical-op result, 0 or 1.
    I1,
    I8,
    I16,
    I32,
    /// 6-byte varnode, seen on some ARM instructions. Fits `u64`, so it is
    /// not a wide integer.
    I48,
    I64,
    /// The x87 ST0/STn bit-pattern view. Stored in `u128` payloads masked to
    /// the low 80 bits. No native Rust type fits, so opt rules needing a
    /// `u64` value get `None` here and skip.
    I80,
    I128,
    I256,
    /// AVX-512 `zmm`. Constants don't fit `u128`, so they intern as
    /// `ConstValue::Wide`.
    I512,
    F32,
    F64,
    /// x87 extended precision (Intel long double). Rust has no `f80`, so opt
    /// never constant-folds F80 arithmetic and the nodes just stay in the IR
    /// for pattern matching. `IntBitsToFloat` / `FloatBitsToInt` skip their
    /// immediate fold too: `FloatConst`'s u64 payload cannot hold 80 bits.
    F80,
}

impl ValueType {
    #[inline]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::I1 => "i1",
            Self::I8 => "i8",
            Self::I16 => "i16",
            Self::I32 => "i32",
            Self::I48 => "i48",
            Self::I64 => "i64",
            Self::I80 => "i80",
            Self::I128 => "i128",
            Self::I256 => "i256",
            Self::I512 => "i512",
            Self::F32 => "f32",
            Self::F64 => "f64",
            Self::F80 => "f80",
        }
    }

    /// `I1` and `I8` both occupy 1 byte.
    #[inline]
    pub fn byte_size(self) -> usize {
        match self {
            Self::I1 | Self::I8 => 1,
            Self::I16 => 2,
            Self::I32 | Self::F32 => 4,
            Self::I48 => 6,
            Self::I64 | Self::F64 => 8,
            Self::I80 | Self::F80 => 10,
            Self::I128 => 16,
            Self::I256 => 32,
            Self::I512 => 64,
        }
    }

    /// `byte_size * 8` for everything except `I1`, which is 1 bit in a byte.
    #[inline]
    pub fn bit_width(self) -> usize {
        match self {
            Self::I1 => 1,
            Self::I8 => 8,
            Self::I16 => 16,
            Self::I32 | Self::F32 => 32,
            Self::I48 => 48,
            Self::I64 | Self::F64 => 64,
            Self::I80 | Self::F80 => 80,
            Self::I128 => 128,
            Self::I256 => 256,
            Self::I512 => 512,
        }
    }

    #[inline]
    pub fn is_bool(self) -> bool {
        self == Self::I1
    }

    #[inline]
    pub fn is_integer(self) -> bool {
        !self.is_float()
    }

    #[inline]
    pub fn is_float(self) -> bool {
        matches!(self, Self::F32 | Self::F64 | Self::F80)
    }

    /// An integer too wide for `u64`. `F80` shares I80's 10-byte size but is
    /// excluded, being a float.
    #[inline]
    pub fn is_wide_int(self) -> bool {
        self.is_integer() && self.byte_size() > 8
    }

    /// All-ones mask for this integer type.
    ///
    /// `I256` / `I512` also return `u128::MAX`: the carrier cannot represent
    /// their width, so this is a conservative approximation, not a rejection.
    /// Masking a genuinely 256-bit value means going through the const
    /// interner's `ConstValue::Wide` limbs instead. Floats return 0
    /// defensively; no caller should ask.
    pub fn bit_mask_u128(self) -> u128 {
        let bits = self.bit_width();
        if bits == 0 || !self.is_integer() {
            return 0;
        }
        if bits >= 128 {
            return u128::MAX;
        }
        (1u128 << bits) - 1
    }

    /// Masks `val` to this type's width, or `None` for floats and for widths
    /// past the `u128` carrier.
    ///
    /// Rejecting > 128 bits keeps this symmetric with [`Self::get_signed_int`]:
    /// a query that can only see the low 128 bits must fail loudly rather than
    /// hand back a silently truncated "success".
    pub fn get_unsigned_int(self, val: u128) -> Option<u128> {
        if !self.is_integer() {
            return None;
        }
        if self.bit_width() > 128 {
            return None;
        }
        Some(val & self.bit_mask_u128())
    }

    /// Sign-extends `val`, read as this type's narrow representation, to a
    /// full `i128`. `None` for floats and for widths past the carrier.
    pub fn get_signed_int(self, val: u128) -> Option<i128> {
        if !self.is_integer() {
            return None;
        }
        let bits = self.bit_width();
        if bits == 0 || bits > 128 {
            return None;
        }
        let masked = val & self.bit_mask_u128();
        if bits == 128 {
            return Some(masked as i128);
        }
        let sign_bit = 1u128 << (bits - 1);
        if (masked & sign_bit) != 0 {
            let high_extension = !((1u128 << bits) - 1);
            Some((masked | high_extension) as i128)
        } else {
            Some(masked as i128)
        }
    }
}

impl ValueType {
    /// Byte size 1 maps to `I8`, never `I1`. Only comparisons and logical ops
    /// produce `I1`; no varnode width does.
    pub fn int_for_byte_size(n: u32) -> crate::error::Result<Self> {
        match n {
            1 => Ok(Self::I8),
            2 => Ok(Self::I16),
            4 => Ok(Self::I32),
            6 => Ok(Self::I48),
            8 => Ok(Self::I64),
            10 => Ok(Self::I80),
            16 => Ok(Self::I128),
            32 => Ok(Self::I256),
            64 => Ok(Self::I512),
            n => Err(anyhow::anyhow!("unsupported node output size: {n} bytes")),
        }
    }

    pub fn float_for_byte_size(n: u32) -> crate::error::Result<Self> {
        match n {
            4 => Ok(Self::F32),
            8 => Ok(Self::F64),
            10 => Ok(Self::F80),
            other => Err(anyhow::anyhow!(
                "unsupported float varnode size {other} bytes (expected 4, 8, or 10)"
            )),
        }
    }
}

impl std::fmt::Display for ValueType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Spares the value-producing path from threading `vn.size` by hand, and gives
/// the "unsupported width" diagnostic one home. Re-exported from the crate root
/// for the lifter.
pub trait VnTypeExt {
    fn int_type(&self) -> crate::error::Result<ValueType>;

    fn float_type(&self) -> crate::error::Result<ValueType>;
}

impl VnTypeExt for rsleigh::Vn {
    #[inline]
    fn int_type(&self) -> crate::error::Result<ValueType> {
        ValueType::int_for_byte_size(self.size)
    }

    #[inline]
    fn float_type(&self) -> crate::error::Result<ValueType> {
        ValueType::float_for_byte_size(self.size)
    }
}
#[cfg(test)]
mod tests {
    use super::ValueType;

    #[test]
    fn bit_mask_u128_widths() {
        assert_eq!(ValueType::I1.bit_mask_u128(), 0x1u128);
        assert_eq!(ValueType::I8.bit_mask_u128(), 0xffu128);
        assert_eq!(ValueType::I16.bit_mask_u128(), 0xffffu128);
        assert_eq!(ValueType::I32.bit_mask_u128(), 0xffff_ffffu128);
        assert_eq!(ValueType::I64.bit_mask_u128(), u64::MAX as u128);
        assert_eq!(ValueType::I128.bit_mask_u128(), u128::MAX);
        assert_eq!(ValueType::F32.bit_mask_u128(), 0);
        assert_eq!(ValueType::F64.bit_mask_u128(), 0);
    }

    #[test]
    fn get_unsigned_int_masks_to_width() {
        assert_eq!(
            ValueType::I16.get_unsigned_int(0x12345678u128),
            Some(0x5678u128)
        );
        assert_eq!(
            ValueType::I32.get_unsigned_int(0x12345678u128),
            Some(0x12345678u128)
        );
        assert_eq!(ValueType::I128.get_unsigned_int(u128::MAX), Some(u128::MAX));
        assert_eq!(ValueType::F32.get_unsigned_int(0x12345678u128), None);
        // I1 masks to the low bit.
        assert_eq!(ValueType::I1.get_unsigned_int(1), Some(1));
        assert_eq!(ValueType::I1.get_unsigned_int(0xFE), Some(0));
    }

    #[test]
    fn get_signed_int_sign_extends_negative_at_narrow_widths() {
        let neg50_at_u32 = 0xffff_ffceu128;
        assert_eq!(ValueType::I32.get_signed_int(neg50_at_u32), Some(-50i128));
        assert_eq!(ValueType::I8.get_signed_int(0xceu128), Some(-50i128));
        assert_eq!(ValueType::I32.get_signed_int(50u128), Some(50i128));
    }

    #[test]
    fn get_signed_int_handles_full_u128_width() {
        let neg1_at_u128 = u128::MAX;
        assert_eq!(ValueType::I128.get_signed_int(neg1_at_u128), Some(-1i128));
        let max_pos = i128::MAX as u128;
        assert_eq!(ValueType::I128.get_signed_int(max_pos), Some(i128::MAX));
    }

    /// The x87 ST0 width; the lifter needs it to set up x86 float functions.
    #[test]
    fn u80_f80_widths() {
        assert_eq!(ValueType::I80.byte_size(), 10);
        assert_eq!(ValueType::I80.bit_width(), 80);
        assert_eq!(ValueType::F80.byte_size(), 10);
        assert_eq!(ValueType::F80.bit_width(), 80);
    }

    /// Drives the validator's local-typing check and the lifter's coercions.
    #[test]
    fn u80_is_integer_and_f80_is_float() {
        assert!(ValueType::I80.is_integer());
        assert!(!ValueType::I80.is_float());
        assert!(ValueType::F80.is_float());
        assert!(!ValueType::F80.is_integer());
    }

    /// Opt rules use this mask for rewrites like `x & all_ones` to `x`.
    #[test]
    fn bit_mask_u128_for_u80() {
        let expected: u128 = (1u128 << 80) - 1;
        assert_eq!(ValueType::I80.bit_mask_u128(), expected);
        assert_eq!(ValueType::F80.bit_mask_u128(), 0);
    }

    #[test]
    fn get_unsigned_int_for_u80_masks_to_80_bits() {
        let raw: u128 = 0xFFFF_FFFF_FFFF_FFFF_FFFF_FFFF_FFFF_FFFFu128;
        let mask: u128 = (1u128 << 80) - 1;
        assert_eq!(ValueType::I80.get_unsigned_int(raw), Some(mask));
        assert_eq!(
            ValueType::I80.get_unsigned_int(0x12345678),
            Some(0x12345678)
        );
    }

    #[test]
    fn get_signed_int_for_u80_sign_extends() {
        let neg1_at_u80 = (1u128 << 80) - 1;
        assert_eq!(ValueType::I80.get_signed_int(neg1_at_u80), Some(-1i128));
        assert_eq!(ValueType::I80.get_signed_int(50u128), Some(50i128));
        let neg50 = ((1u128 << 80) - 1) ^ 49;
        assert_eq!(ValueType::I80.get_signed_int(neg50), Some(-50i128));
    }

    /// The lifter's varnode-to-type conversion must handle x87 80-bit regs.
    #[test]
    fn int_for_byte_size_10_is_i80() {
        let ty = ValueType::int_for_byte_size(10).expect("10 must convert to I80");
        assert_eq!(ty, ValueType::I80);
    }

    #[test]
    fn int_for_byte_size_maps_widths() {
        use super::ValueType as T;
        assert_eq!(T::int_for_byte_size(1).unwrap(), T::I8);
        assert_eq!(T::int_for_byte_size(6).unwrap(), T::I48);
        assert_eq!(T::int_for_byte_size(8).unwrap(), T::I64);
        assert_eq!(T::int_for_byte_size(64).unwrap(), T::I512);
        assert!(T::int_for_byte_size(3).is_err());

        // I48 fits u64, so it is not wide, and its mask/sign helpers come off
        // the generic bit_width path.
        assert_eq!(T::I48.byte_size(), 6);
        assert_eq!(T::I48.bit_width(), 48);
        assert!(!T::I48.is_wide_int());
        assert_eq!(T::I48.bit_mask_u128(), (1u128 << 48) - 1);
        assert_eq!(T::I48.get_signed_int((1u128 << 48) - 1), Some(-1i128));
    }

    /// Pins the `bits >= 128` guard: past the carrier, the mask approximates.
    #[test]
    fn bit_mask_u128_for_u256_and_u512_is_u128_max() {
        assert_eq!(ValueType::I256.bit_mask_u128(), u128::MAX);
        assert_eq!(ValueType::I512.bit_mask_u128(), u128::MAX);
    }

    /// A > 128-bit query must fail loudly rather than return the low 128 bits
    /// as a success, symmetric with `get_signed_int`.
    #[test]
    fn get_unsigned_int_i256_does_not_falsely_succeed() {
        assert_eq!(ValueType::I256.get_unsigned_int(0xDEAD_BEEFu128), None);
        assert_eq!(ValueType::I256.get_unsigned_int(u128::MAX), None);
        assert_eq!(ValueType::I512.get_unsigned_int(42u128), None);
        assert_eq!(ValueType::I256.get_signed_int(0xDEAD_BEEFu128), None);
        assert_eq!(ValueType::I512.get_signed_int(42u128), None);
        // Exactly 128 bits still fits the carrier.
        assert_eq!(ValueType::I128.get_unsigned_int(u128::MAX), Some(u128::MAX));
    }
}
