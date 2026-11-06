use anyhow::anyhow;

/// An integer or IEEE float type, by width in bits. Only [`Self::SUPPORTED`]
/// widths are valid. Integers order before floats, each by width.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ValueType {
    Int(u16),
    Float(u16),
}

macro_rules! supported_widths {
    (int: $($i:literal)*; float: $($f:literal)*) => {
        impl ValueType {
            /// Every type, in `Ord` order.
            pub const SUPPORTED: [Self; [$($i,)* $($f,)*].len()] =
                [$(Self::Int($i),)* $(Self::Float($f),)*];

            #[inline]
            pub(crate) const fn is_supported(self) -> bool {
                match self {
                    Self::Int(w) => matches!(w, $($i)|*),
                    Self::Float(w) => matches!(w, $($f)|*),
                }
            }
        }
    };
}

// Every width some vendored sla spec gives a varnode or temporary (i24: x86
// `tmp:3`, i72: x86 `adcx`, i80: x87 ST).
supported_widths! {
    int: 1 8 16 24 32 40 48 56 64 72 80 96 112 128 256 512;
    float: 16 32 64 80 128
}

impl ValueType {
    /// Boolean.
    pub const I1: Self = Self::Int(1);

    /// Panics on an unsupported width (a compile error in a `const`).
    #[inline]
    pub const fn int(bits: u16) -> Self {
        let ty = Self::Int(bits);
        assert!(ty.is_supported(), "unsupported integer width");
        ty
    }

    /// Panics on an unsupported width.
    #[inline]
    pub const fn float(bits: u16) -> Self {
        let ty = Self::Float(bits);
        assert!(ty.is_supported(), "unsupported float width");
        ty
    }

    /// One byte is `I8`, never `I1`.
    pub fn int_from_bytes(n: u32) -> crate::Result<Self> {
        Self::from_bytes(n, Self::Int)
            .ok_or_else(|| anyhow!("unsupported integer varnode size: {n} bytes"))
    }

    pub(crate) fn float_from_bytes(n: u32) -> crate::Result<Self> {
        Self::from_bytes(n, Self::Float)
            .ok_or_else(|| anyhow!("unsupported float varnode size: {n} bytes"))
    }

    fn from_bytes(n: u32, of_bits: impl Fn(u16) -> Self) -> Option<Self> {
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

    /// The all-ones value. `None` for a float or past 128 bits.
    #[inline]
    pub const fn int_mask_u128(self) -> Option<u128> {
        match self {
            Self::Int(w) if w <= 128 => Some(crate::eval::mask_below(w as u32)),
            Self::Int(_) | Self::Float(_) => None,
        }
    }

    /// Masks raw IEEE 754 bits to this width; 64 bits and wider pass through.
    pub fn mask_float_bits(self, bits: u64) -> u64 {
        (u128::from(bits) & crate::eval::mask_below(self.bit_width() as u32)) as u64
    }

    /// Sign-extends `val` from this width to `i128`. `None` for floats and past
    /// 128 bits.
    pub fn get_signed_int(self, val: u128) -> Option<i128> {
        let Self::Int(w @ 1..=128) = self else {
            return None;
        };
        let spare = 128 - u32::from(w);
        Some(((val << spare) as i128) >> spare)
    }

    /// Inverse of `Display`.
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

/// `I32` / `F64`.
impl std::fmt::Debug for ValueType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Int(w) => write!(f, "I{w}"),
            Self::Float(w) => write!(f, "F{w}"),
        }
    }
}

/// `i32` / `f64`.
impl std::fmt::Display for ValueType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Int(w) => write!(f, "i{w}"),
            Self::Float(w) => write!(f, "f{w}"),
        }
    }
}

/// Inverse of `Display`, lowercase only.
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
    fn int_type(&self) -> crate::Result<ValueType>;

    fn float_type(&self) -> crate::Result<ValueType>;
}

impl VnTypeExt for rsleigh::Vn {
    #[inline]
    fn int_type(&self) -> crate::Result<ValueType> {
        ValueType::int_from_bytes(self.size)
    }

    #[inline]
    fn float_type(&self) -> crate::Result<ValueType> {
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
        for bad in [
            "", "i", "i12", "i08", "I32", "f8", "u32", "i32 ", "i+8", "i0", "i 8", "i8_", "f99999",
            "\u{e9}8",
        ] {
            let err = bad.parse::<ValueType>().expect_err(bad);
            let msg = err.to_string();
            assert!(
                msg.starts_with(&format!("unknown value type {bad:?}: ")),
                "{msg}"
            );
            assert!(
                msg.ends_with(
                    "expected one of i1, i8, i16, i24, i32, i40, \
                     i48, i56, i64, i72, i80, i96, i112, i128, i256, i512, f16, f32, f64, \
                     f80, f128"
                ),
                "{bad:?}: {msg}"
            );
        }
        for n in [0u32, 11, 13, 15, 17, 33, 65, u32::MAX] {
            let err = ValueType::int_from_bytes(n).expect_err("unlisted size");
            assert!(
                err.to_string()
                    .contains(&format!("unsupported integer varnode size: {n} bytes")),
                "{err}"
            );
        }
        for n in [0u32, 1, 3, 12, 32] {
            let err = ValueType::float_from_bytes(n).expect_err("unlisted size");
            assert!(
                err.to_string()
                    .contains(&format!("unsupported float varnode size: {n} bytes")),
                "{err}"
            );
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
    fn mask_float_bits_keeps_the_declared_width() {
        assert_eq!(ValueType::float(16).mask_float_bits(u64::MAX), 0xffff);
        assert_eq!(ValueType::float(32).mask_float_bits(u64::MAX), 0xffff_ffff);
        for w in [64, 80, 128] {
            assert_eq!(ValueType::float(w).mask_float_bits(u64::MAX), u64::MAX);
        }
        assert_eq!(ValueType::Float(0).mask_float_bits(u64::MAX), 0);
        assert_eq!(ValueType::I1.mask_float_bits(u64::MAX), 1);
    }

    /// A float has no integer bits.
    #[test]
    fn mask_at_a_float_type_is_zero() {
        let v = dashu_int::UBig::from(u128::MAX);
        for w in [32, 64, 80] {
            assert_eq!(
                crate::eval::mask(&v, ValueType::float(w)),
                dashu_int::UBig::ZERO
            );
        }
    }

    /// Agrees with `eval::signed` at every width the carrier holds, on
    /// unmasked inputs too; `None` past the carrier, at `Int(0)` and for a float.
    #[test]
    fn get_signed_int_agrees_with_eval_signed() {
        use dashu_int::UBig;
        for ty in ValueType::SUPPORTED {
            let w = ty.bit_width();
            let sign = 1u128.checked_shl(w.saturating_sub(1) as u32).unwrap_or(0);
            for v in [
                0,
                1,
                sign,
                sign.wrapping_sub(1),
                ty.int_mask_u128().unwrap_or(0),
                u128::MAX,
            ] {
                let want = match ty {
                    ValueType::Int(1..=128) => {
                        crate::eval::signed(&UBig::from(v), ty).map(|s| i128::try_from(s).unwrap())
                    }
                    ValueType::Int(_) | ValueType::Float(_) => None,
                };
                assert_eq!(ty.get_signed_int(v), want, "{ty} {v:#x}");
            }
        }
        assert_eq!(ValueType::Int(0).get_signed_int(u128::MAX), None);
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
        assert!(T::int(48).bit_width() <= 64);
        assert_eq!(T::int(48).int_mask_u128(), Some((1u128 << 48) - 1));
        assert_eq!(T::int(48).get_signed_int((1u128 << 48) - 1), Some(-1i128));
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
        }
    }

    /// The unsigned read has no carrier, so a > 128-bit width answers in
    /// full; the signed one still fails loudly rather than return the low
    /// 128 bits as a success.
    #[test]
    fn past_the_carrier_only_the_signed_read_refuses() {
        use crate::eval::mask;
        use dashu_int::UBig;
        let v = UBig::from(0xDEAD_BEEFu32);
        assert_eq!(mask(&v, ValueType::int(256)), v);
        let wide = UBig::ONE << 200usize;
        assert_eq!(mask(&wide, ValueType::int(512)), wide);
        assert_eq!(ValueType::int(256).get_signed_int(0xDEAD_BEEFu128), None);
        assert_eq!(ValueType::int(512).get_signed_int(42u128), None);
        // Exactly 128 bits still fits the signed carrier.
        assert_eq!(ValueType::int(128).get_signed_int(u128::MAX), Some(-1));
    }
}
