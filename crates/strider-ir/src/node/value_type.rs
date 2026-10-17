use anyhow::anyhow;

/// A value's type: an integer or an IEEE float, by its width in bits.
///
/// Only [`Self::SUPPORTED`] widths are valid: [`Self::int`], [`Self::float`],
/// the `*_from_bytes` constructors and `FromStr` refuse any other, and
/// `validate` rejects a value carrying one. Integers order before floats,
/// each by width.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ValueType {
    Int(u16),
    Float(u16),
}

// `SUPPORTED` and `is_supported` read one width list.
macro_rules! supported_widths {
    (int: $($i:literal)*; float: $($f:literal)*) => {
        impl ValueType {
            /// Every type: integers, then floats, each by width (`Ord`
            /// order).
            pub const SUPPORTED: [Self; [$($i,)* $($f,)*].len()] =
                [$(Self::Int($i),)* $(Self::Float($f),)*];

            /// Whether this width is one of [`Self::SUPPORTED`].
            #[inline]
            pub const fn is_supported(self) -> bool {
                match self {
                    Self::Int(w) => matches!(w, $($i)|*),
                    Self::Float(w) => matches!(w, $($f)|*),
                }
            }
        }
    };
}

// Past the powers of two, each width is one a vendored sla spec puts on a
// varnode or temporary: i24 x86 `SegmentLimit` (`tmp:3`); i40 and i72 the x86
// `adcx`/`adox` carry accumulators (`tmp:5`, `tmp:9`), i72 also AArch64
// fixed-point `ucvtf`; i48 some ARM instructions; i56 AArch64 `cmpp`/`subp`;
// i80 the x87 ST bit pattern; i96 and i112 x86-64 `GDTR`/`IDTR` and
// `LDTR`/`TR`; i512 AVX-512 `zmm`; f16 AArch64 FPR16 and ARM `vcvt.f16`; f80
// x87 extended precision; f128 the destination of ARM `vcvt.f32.f16 Qd,Dm`.
supported_widths! {
    int: 1 8 16 24 32 40 48 56 64 72 80 96 112 128 256 512;
    float: 16 32 64 80 128
}

impl ValueType {
    /// The boolean type: a comparison or logical-op result, 0 or 1.
    pub const I1: Self = Self::Int(1);

    /// The integer type `bits` wide. Panics on an unsupported width, which in
    /// a `const` is a compile error.
    #[inline]
    pub const fn int(bits: u16) -> Self {
        let ty = Self::Int(bits);
        assert!(ty.is_supported(), "unsupported integer width");
        ty
    }

    /// The float type `bits` wide; panics as [`Self::int`] does.
    #[inline]
    pub const fn float(bits: u16) -> Self {
        let ty = Self::Float(bits);
        assert!(ty.is_supported(), "unsupported float width");
        ty
    }

    /// The integer type of an `n`-byte varnode: one byte is `I8`, never `I1`.
    pub fn int_from_bytes(n: u32) -> crate::error::Result<Self> {
        Self::from_bytes(n, Self::Int)
            .ok_or_else(|| anyhow!("unsupported node output size: {n} bytes"))
    }

    /// The float type of an `n`-byte varnode.
    pub fn float_from_bytes(n: u32) -> crate::error::Result<Self> {
        Self::from_bytes(n, Self::Float)
            .ok_or_else(|| anyhow!("unsupported float varnode size: {n} bytes"))
    }

    fn from_bytes(n: u32, of_bits: fn(u16) -> Self) -> Option<Self> {
        let ty = of_bits(u16::try_from(n.checked_mul(8)?).ok()?);
        ty.is_supported().then_some(ty)
    }

    #[inline]
    pub const fn bit_width(self) -> usize {
        match self {
            Self::Int(w) | Self::Float(w) => w as usize,
        }
    }

    /// `I1` and `I8` both occupy 1 byte.
    #[inline]
    pub const fn byte_size(self) -> usize {
        self.bit_width().div_ceil(8)
    }

    #[inline]
    pub const fn is_bool(self) -> bool {
        matches!(self, Self::I1)
    }

    #[inline]
    pub const fn is_integer(self) -> bool {
        matches!(self, Self::Int(_))
    }

    #[inline]
    pub const fn is_float(self) -> bool {
        matches!(self, Self::Float(_))
    }

    /// An integer too wide for `u64`.
    #[inline]
    pub const fn is_wide_int(self) -> bool {
        matches!(self, Self::Int(w) if w > 64)
    }

    /// `v` reduced to this type's width, `v mod 2^width`: exact at every
    /// integer width, since past 128 bits a `u128` is already below `2^width`.
    /// A float has no integer bits: 0.
    #[inline]
    pub const fn wrap_u128(self, v: u128) -> u128 {
        match self {
            Self::Int(w) => v & low_bits_mask_u128(w as usize),
            Self::Float(_) => 0,
        }
    }

    /// This integer type's all-ones value, the modulus less one. `None` for a
    /// float, and past the `u128` carrier, where no `u128` is that value.
    #[inline]
    pub const fn int_mask_u128(self) -> Option<u128> {
        match self {
            Self::Int(w) if w <= 128 => Some(low_bits_mask_u128(w as usize)),
            Self::Int(_) | Self::Float(_) => None,
        }
    }

    /// Masks a raw IEEE 754 pattern to this float type's width. Widths at or
    /// past the `u64` carrier pass `bits` through unchanged.
    pub fn mask_float_bits(self, bits: u64) -> u64 {
        match self.bit_width() {
            w if w < 64 => bits & ((1u64 << w) - 1),
            _ => bits,
        }
    }

    /// Masks `val` to this type's width, or `None` for floats and for widths
    /// past the `u128` carrier.
    pub fn get_unsigned_int(self, val: u128) -> Option<u128> {
        Some(val & self.int_mask_u128()?)
    }

    /// Sign-extends `val`, read as this type's narrow representation, to a
    /// full `i128`. `None` for floats and for widths past the carrier.
    pub fn get_signed_int(self, val: u128) -> Option<i128> {
        let mask = self.int_mask_u128()?;
        let bits = self.bit_width();
        if bits == 0 {
            return None;
        }
        let masked = val & mask;
        if bits == 128 {
            return Some(masked as i128);
        }
        let sign_bit = 1u128 << (bits - 1);
        if (masked & sign_bit) != 0 {
            Some((masked | !mask) as i128)
        } else {
            Some(masked as i128)
        }
    }

    /// The `Display` spelling read back; `None` for any other text.
    fn parse(s: &str) -> Option<Self> {
        let (of_bits, digits): (fn(u16) -> Self, &str) = match s.split_at_checked(1)? {
            ("i", digits) => (Self::Int, digits),
            ("f", digits) => (Self::Float, digits),
            _ => return None,
        };
        // `u16::from_str` also takes `+8` and `08`, which `Display` never writes.
        if digits.starts_with('0') || !digits.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let ty = of_bits(digits.parse().ok()?);
        ty.is_supported().then_some(ty)
    }
}

/// The low `bits` bits of the `u128` carrier, saturating to `u128::MAX` at
/// 128 and past: a raw bit helper. A type's own mask is
/// [`ValueType::int_mask_u128`].
#[inline]
pub const fn low_bits_mask_u128(bits: usize) -> u128 {
    if bits >= 128 {
        u128::MAX
    } else {
        (1u128 << bits) - 1
    }
}

/// `I32` / `F64`: the spelling Python's `Node.value_type()` returns.
impl std::fmt::Debug for ValueType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Int(w) => write!(f, "I{w}"),
            Self::Float(w) => write!(f, "F{w}"),
        }
    }
}

/// `i32` / `f64`: the IR text spelling.
impl std::fmt::Display for ValueType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Int(w) => write!(f, "i{w}"),
            Self::Float(w) => write!(f, "f{w}"),
        }
    }
}

/// The inverse of `Display`: `"i32"`, `"f64"`, lowercase only.
impl std::str::FromStr for ValueType {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s).ok_or_else(|| {
            let names = Self::SUPPORTED.map(|ty| ty.to_string());
            anyhow!(
                "unknown value type {s:?}: expected one of {}",
                names.join(", ")
            )
        })
    }
}

/// The [`ValueType`] a varnode's byte size maps to.
pub trait VnTypeExt {
    fn int_type(&self) -> crate::error::Result<ValueType>;

    fn float_type(&self) -> crate::error::Result<ValueType>;
}

impl VnTypeExt for rsleigh::Vn {
    #[inline]
    fn int_type(&self) -> crate::error::Result<ValueType> {
        ValueType::int_from_bytes(self.size)
    }

    #[inline]
    fn float_type(&self) -> crate::error::Result<ValueType> {
        ValueType::float_from_bytes(self.size)
    }
}

#[cfg(test)]
mod tests {
    use super::ValueType;

    /// Every spelling and size fact agrees, type by type, and the list is
    /// exactly the widths the vendored sla specs carry.
    #[test]
    fn supported_types_agree_on_spelling_and_size() {
        let names: Vec<String> = ValueType::SUPPORTED
            .iter()
            .map(ToString::to_string)
            .collect();
        assert_eq!(
            names.join(" "),
            "i1 i8 i16 i24 i32 i40 i48 i56 i64 i72 i80 i96 i112 i128 i256 i512 \
             f16 f32 f64 f80 f128"
        );
        assert!(ValueType::SUPPORTED.is_sorted());
        for ty in ValueType::SUPPORTED {
            assert_eq!(format!("{ty:?}").to_lowercase(), ty.to_string());
            assert_eq!(ty.to_string().parse::<ValueType>().unwrap(), ty);
            assert_eq!(ty.is_float(), ty.to_string().starts_with('f'));
            assert_eq!(ty.is_integer(), !ty.is_float());
            assert_eq!(ty.is_bool(), ty == ValueType::I1);
            assert_eq!(ty.is_wide_int(), ty.is_integer() && ty.bit_width() > 64);
            assert_eq!(ty.byte_size(), ty.bit_width().div_ceil(8));
            let bits = u16::try_from(ty.bit_width()).unwrap();
            let bytes = u32::try_from(ty.byte_size()).unwrap();
            if ty.is_float() {
                assert_eq!(ValueType::float(bits), ty);
                assert_eq!(ValueType::float_from_bytes(bytes).unwrap(), ty);
            } else {
                assert_eq!(ValueType::int(bits), ty);
                if !ty.is_bool() {
                    assert_eq!(ValueType::int_from_bytes(bytes).unwrap(), ty);
                }
            }
        }
    }

    /// The parser takes exactly the `Display` spelling; sizes off the list err.
    #[test]
    fn parse_and_from_bytes_refuse_unlisted_types() {
        for bad in ["", "i", "i12", "i08", "I32", "f8", "u32", "i32 "] {
            let err = bad.parse::<ValueType>().expect_err(bad);
            assert!(
                err.to_string().ends_with(
                    "expected one of i1, i8, i16, i24, i32, i40, \
                     i48, i56, i64, i72, i80, i96, i112, i128, i256, i512, f16, f32, f64, \
                     f80, f128"
                ),
                "{bad:?}: {err}"
            );
        }
        for n in [0u32, 11, 13, 15, 17, 33, 65, u32::MAX] {
            let err = ValueType::int_from_bytes(n).expect_err("unlisted size");
            assert!(
                err.to_string()
                    .contains(&format!("unsupported node output size: {n} bytes")),
                "{err}"
            );
        }
        for n in [0u32, 1, 3, 12, 32] {
            assert!(ValueType::float_from_bytes(n).is_err(), "{n}");
        }
    }

    /// A type is its kind and width; only the listed widths are supported.
    #[test]
    fn a_type_is_a_kind_and_a_width() {
        assert_eq!(ValueType::int(32), ValueType::Int(32));
        assert_eq!(ValueType::float(64), ValueType::Float(64));
        assert_eq!(ValueType::I1, ValueType::Int(1));
        assert_eq!(
            format!("{:?} {:?}", ValueType::I1, ValueType::Float(80)),
            "I1 F80"
        );
        assert!(ValueType::SUPPORTED.iter().all(|ty| ty.is_supported()));
        for ty in [ValueType::Int(0), ValueType::Int(12), ValueType::Float(8)] {
            assert!(!ty.is_supported(), "{ty:?}");
        }
        for bad in ["i+8", "i0", "i 8", "i8_", "f99999", "\u{e9}8"] {
            assert!(bad.parse::<ValueType>().is_err(), "{bad:?}");
        }
    }

    #[test]
    #[should_panic(expected = "unsupported integer width")]
    fn int_refuses_an_unsupported_width() {
        let _ = ValueType::int(12);
    }

    #[test]
    #[should_panic(expected = "unsupported float width")]
    fn float_refuses_an_unsupported_width() {
        let _ = ValueType::float(8);
    }

    /// Every byte size the vendored x86/ARM/AARCH64/MIPS/PowerPC specs can put
    /// on a p-code varnode.
    #[test]
    fn sleigh_varnode_widths_round_trip() {
        for n in [1u32, 2, 3, 4, 5, 6, 7, 8, 9, 10, 12, 14, 16, 32, 64] {
            let ty =
                ValueType::int_from_bytes(n).unwrap_or_else(|e| panic!("int_from_bytes({n}): {e}"));
            assert_eq!(ty.byte_size(), n as usize);
            assert!(ty.is_integer());
        }
        assert_eq!(ValueType::int_from_bytes(1).unwrap(), ValueType::int(8));

        for n in [2u32, 4, 8, 10, 16] {
            let ty = ValueType::float_from_bytes(n)
                .unwrap_or_else(|e| panic!("float_from_bytes({n}): {e}"));
            assert_eq!(ty.byte_size(), n as usize);
            assert!(ty.is_float());
        }
    }

    #[test]
    fn get_unsigned_int_masks_to_width() {
        assert_eq!(
            ValueType::int(16).get_unsigned_int(0x12345678u128),
            Some(0x5678u128)
        );
        assert_eq!(
            ValueType::int(32).get_unsigned_int(0x12345678u128),
            Some(0x12345678u128)
        );
        assert_eq!(
            ValueType::int(128).get_unsigned_int(u128::MAX),
            Some(u128::MAX)
        );
        assert_eq!(ValueType::float(32).get_unsigned_int(0x12345678u128), None);
        // Every bit above the declared width clears.
        assert_eq!(
            ValueType::int(8).get_unsigned_int(u128::MAX),
            Some(u128::from(u8::MAX))
        );
        assert_eq!(
            ValueType::int(64).get_unsigned_int(u128::MAX),
            Some(u128::from(u64::MAX))
        );
        // I1 masks to the low bit.
        assert_eq!(ValueType::I1.get_unsigned_int(1), Some(1));
        assert_eq!(ValueType::I1.get_unsigned_int(0), Some(0));
        assert_eq!(ValueType::I1.get_unsigned_int(0xFE), Some(0));
    }

    #[test]
    fn get_signed_int_sign_extends_negative_at_narrow_widths() {
        let neg50_at_u32 = 0xffff_ffceu128;
        assert_eq!(
            ValueType::int(32).get_signed_int(neg50_at_u32),
            Some(-50i128)
        );
        assert_eq!(ValueType::int(8).get_signed_int(0xceu128), Some(-50i128));
        assert_eq!(ValueType::int(32).get_signed_int(50u128), Some(50i128));
    }

    #[test]
    fn get_signed_int_handles_full_u128_width() {
        let neg1_at_u128 = u128::MAX;
        assert_eq!(
            ValueType::int(128).get_signed_int(neg1_at_u128),
            Some(-1i128)
        );
        let max_pos = i128::MAX as u128;
        assert_eq!(ValueType::int(128).get_signed_int(max_pos), Some(i128::MAX));
    }

    #[test]
    fn get_unsigned_int_for_u80_masks_to_80_bits() {
        let raw: u128 = 0xFFFF_FFFF_FFFF_FFFF_FFFF_FFFF_FFFF_FFFFu128;
        let mask: u128 = (1u128 << 80) - 1;
        assert_eq!(ValueType::int(80).get_unsigned_int(raw), Some(mask));
        assert_eq!(
            ValueType::int(80).get_unsigned_int(0x12345678),
            Some(0x12345678)
        );
    }

    #[test]
    fn get_signed_int_for_u80_sign_extends() {
        let neg1_at_u80 = (1u128 << 80) - 1;
        assert_eq!(ValueType::int(80).get_signed_int(neg1_at_u80), Some(-1i128));
        assert_eq!(ValueType::int(80).get_signed_int(50u128), Some(50i128));
        let neg50 = ((1u128 << 80) - 1) ^ 49;
        assert_eq!(ValueType::int(80).get_signed_int(neg50), Some(-50i128));
    }

    #[test]
    fn int_from_bytes_10_is_i80() {
        let ty = ValueType::int_from_bytes(10).expect("10 must convert to I80");
        assert_eq!(ty, ValueType::int(80));
    }

    #[test]
    fn int_from_bytes_maps_widths() {
        use super::ValueType as T;
        assert_eq!(T::int_from_bytes(1).unwrap(), T::int(8));
        assert_eq!(T::int_from_bytes(6).unwrap(), T::int(48));
        assert_eq!(T::int_from_bytes(8).unwrap(), T::int(64));
        assert_eq!(T::int_from_bytes(64).unwrap(), T::int(512));
        assert_eq!(T::int_from_bytes(3).unwrap(), T::int(24));

        // I48 fits u64, so it is not wide.
        assert_eq!(T::int(48).byte_size(), 6);
        assert_eq!(T::int(48).bit_width(), 48);
        assert!(!T::int(48).is_wide_int());
        assert_eq!(T::int(48).int_mask_u128(), Some((1u128 << 48) - 1));
        assert_eq!(T::int(48).get_signed_int((1u128 << 48) - 1), Some(-1i128));
    }

    /// Exact at every width: past the carrier a `u128` is already below
    /// `2^width`. A float has no integer bits.
    #[test]
    fn wrap_u128_reduces_to_the_width() {
        assert_eq!(ValueType::I1.wrap_u128(0b11), 1);
        assert_eq!(ValueType::int(24).wrap_u128(u128::MAX), 0xFF_FFFF);
        assert_eq!(ValueType::int(80).wrap_u128(u128::MAX), (1u128 << 80) - 1);
        assert_eq!(ValueType::int(128).wrap_u128(u128::MAX), u128::MAX);
        assert_eq!(ValueType::int(256).wrap_u128(u128::MAX), u128::MAX);
        assert_eq!(ValueType::float(32).wrap_u128(0x3F80_0000), 0);
    }

    /// The all-ones value up to the carrier, and nothing for a float or a
    /// wider integer, never the saturated `u128::MAX`.
    #[test]
    fn int_mask_u128_is_all_ones_up_to_the_carrier() {
        for ty in ValueType::SUPPORTED {
            let want = if ty.is_integer() && ty.bit_width() <= 128 {
                Some(u128::MAX >> (128 - ty.bit_width()))
            } else {
                None
            };
            assert_eq!(ty.int_mask_u128(), want, "{ty:?}");
            if let Some(mask) = want {
                assert_eq!(ty.wrap_u128(u128::MAX), mask, "{ty:?}");
            }
        }
    }

    /// A > 128-bit query must fail loudly rather than return the low 128 bits
    /// as a success.
    #[test]
    fn get_unsigned_int_i256_does_not_falsely_succeed() {
        assert_eq!(ValueType::int(256).get_unsigned_int(0xDEAD_BEEFu128), None);
        assert_eq!(ValueType::int(256).get_unsigned_int(u128::MAX), None);
        assert_eq!(ValueType::int(512).get_unsigned_int(42u128), None);
        assert_eq!(ValueType::int(256).get_signed_int(0xDEAD_BEEFu128), None);
        assert_eq!(ValueType::int(512).get_signed_int(42u128), None);
        // Exactly 128 bits still fits the carrier.
        assert_eq!(
            ValueType::int(128).get_unsigned_int(u128::MAX),
            Some(u128::MAX)
        );
    }
}
