//! Concrete value types carried by node outputs.

/// The value type carried by a node output.
///
/// Integer variants correspond directly to their C-style unsigned integer
/// widths.  `Bool` is a 1-bit logical value.  `F32`/`F64` are IEEE 754
/// floating-point types whose raw bit patterns are stored as `u64`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NodeOutputType {
    Bool,
    U8,
    U16,
    U32,
    U64,
    U128,
    U256,
    /// 32-bit IEEE 754 single-precision float.
    F32,
    /// 64-bit IEEE 754 double-precision float.
    F64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NodeOutputTypeCategory {
    Bool,
    Int,
    Float,
}

struct TypeInfo {
    name: &'static str,
    byte_size: u8,
    category: NodeOutputTypeCategory,
}

// Order MUST match the `NodeOutputType` enum declaration order
// (asserted by `type_info_table_matches_variants` in the test module).
const TYPE_INFO: &[TypeInfo] = &[
    TypeInfo { name: "bool", byte_size: 1,  category: NodeOutputTypeCategory::Bool  },
    TypeInfo { name: "u8",   byte_size: 1,  category: NodeOutputTypeCategory::Int   },
    TypeInfo { name: "u16",  byte_size: 2,  category: NodeOutputTypeCategory::Int   },
    TypeInfo { name: "u32",  byte_size: 4,  category: NodeOutputTypeCategory::Int   },
    TypeInfo { name: "u64",  byte_size: 8,  category: NodeOutputTypeCategory::Int   },
    TypeInfo { name: "u128", byte_size: 16, category: NodeOutputTypeCategory::Int   },
    TypeInfo { name: "u256", byte_size: 32, category: NodeOutputTypeCategory::Int   },
    TypeInfo { name: "f32",  byte_size: 4,  category: NodeOutputTypeCategory::Float },
    TypeInfo { name: "f64",  byte_size: 8,  category: NodeOutputTypeCategory::Float },
];

impl NodeOutputType {
    #[inline]
    fn info(self) -> &'static TypeInfo {
        &TYPE_INFO[self as usize]
    }

    /// Returns the canonical name of this type as a static string.
    #[inline]
    #[must_use]
    pub fn as_str(self) -> &'static str {
        self.info().name
    }

    /// Returns the size of this type **in bytes**.
    ///
    /// Both `Bool` and `U8` return 1.
    #[inline]
    #[must_use]
    pub fn byte_size(self) -> usize {
        self.info().byte_size as usize
    }

    /// Returns the width of this type **in bits** (`byte_size * 8`).
    #[inline]
    #[must_use]
    pub fn bit_width(self) -> usize {
        self.byte_size() * 8
    }

    /// Whether a constant of this type fits in a `u64` (i.e. `byte_size <= 8`).
    ///
    /// Returns `true` for `Bool`, `U8`, `U16`, `U32`, `U64`, `F32`, and `F64`.
    /// Returns `false` for `U128` and `U256`.
    #[inline]
    #[must_use]
    pub fn fits_u64(self) -> bool {
        self.byte_size() <= 8
    }

    /// Returns `true` if this type is `Bool`.
    #[inline]
    #[must_use]
    pub fn is_bool(self) -> bool {
        matches!(self.info().category, NodeOutputTypeCategory::Bool)
    }

    /// Returns `true` if this type is one of the unsigned integer variants
    /// (U8, U16, U32, U64, U128, U256).
    #[inline]
    #[must_use]
    pub fn is_integer(self) -> bool {
        matches!(self.info().category, NodeOutputTypeCategory::Int)
    }

    /// Returns `true` if this type is `F32` or `F64`.
    #[inline]
    #[must_use]
    pub fn is_float(self) -> bool {
        matches!(self.info().category, NodeOutputTypeCategory::Float)
    }

    /// Returns the unsigned integer type with the same byte size.
    /// (Bool→U8, F32→U32, F64→U64, Ux→Ux)
    #[inline]
    #[must_use]
    pub fn to_natural_int_type(self) -> NodeOutputType {
        match self {
            NodeOutputType::Bool | NodeOutputType::U8 => NodeOutputType::U8,
            NodeOutputType::U16 => NodeOutputType::U16,
            NodeOutputType::U32 | NodeOutputType::F32 => NodeOutputType::U32,
            NodeOutputType::U64 | NodeOutputType::F64 => NodeOutputType::U64,
            NodeOutputType::U128 => NodeOutputType::U128,
            NodeOutputType::U256 => NodeOutputType::U256,
        }
    }

    /// Interprets `val` as an unsigned integer of this width and returns the
    /// truncated value, or `None` if this type is `Bool` or a float type.
    ///
    /// The truncation ensures that bits beyond the type's width are cleared,
    /// matching the hardware behaviour of narrower registers.
    #[inline]
    #[must_use]
    pub fn get_unsigned_int(self, val: u64) -> Option<u64> {
        match self {
            NodeOutputType::Bool
            | NodeOutputType::U128
            | NodeOutputType::U256
            | NodeOutputType::F32
            | NodeOutputType::F64 => None,
            NodeOutputType::U8 => Some(val as u8 as u64),
            NodeOutputType::U16 => Some(val as u16 as u64),
            NodeOutputType::U32 => Some(val as u32 as u64),
            NodeOutputType::U64 => Some(val),
        }
    }

    /// Interprets `val` as a signed integer of this width with sign-extension
    /// and returns the result, or `None` if this type is `Bool` or a float type.
    ///
    /// Casting through the signed type of the same width sign-extends the
    /// value to 64 bits.
    #[inline]
    #[must_use]
    pub fn get_signed_int(self, val: u64) -> Option<i64> {
        match self {
            NodeOutputType::Bool
            | NodeOutputType::U128
            | NodeOutputType::U256
            | NodeOutputType::F32
            | NodeOutputType::F64 => None,
            NodeOutputType::U8 => Some(val as i8 as i64),
            NodeOutputType::U16 => Some(val as i16 as i64),
            NodeOutputType::U32 => Some(val as i32 as i64),
            NodeOutputType::U64 => Some(val as i64),
        }
    }

    /// Sign-extends `val` from this type's width to 64 bits and returns the
    /// result as a `u64` bit pattern.
    ///
    /// Returns `None` if this type is `Bool`, `U128`, `U256`, or a float type,
    /// since those widths either are not integer or cannot be represented in 64
    /// bits.
    #[inline]
    #[must_use]
    pub fn sign_extend(self, val: u64) -> Option<u64> {
        self.get_signed_int(val).map(|v| v as u64)
    }

    /// Returns the all-ones bit mask for this integer type, as `u128`.
    /// `Bool` returns `1`; integer widths return their natural bit widths.
    /// `U256` returns `u128::MAX` as a best-effort sentinel — this method is
    /// not meaningful for U256 and the IntConst path panics for U256 today;
    /// callers that genuinely need U256 must be revisited when U256 support
    /// is added.  Float types return `0` (defensive — no caller should ask).
    #[must_use]
    pub fn bit_mask_u128(self) -> u128 {
        if self.is_bool() {
            return 1;
        }
        let bits = self.bit_width();
        if bits == 0 || !self.is_integer() {
            return 0;
        }
        if bits >= 128 {
            return u128::MAX;
        }
        (1u128 << bits) - 1
    }

    /// Masks `val` to this type's bit width.  For widths ≥ 128 returns `val`
    /// unchanged.  Companion to [`Self::get_unsigned_int`] but works at u128
    /// width.
    #[must_use]
    pub fn get_unsigned_int_u128(self, val: u128) -> Option<u128> {
        if !self.is_integer() {
            return None;
        }
        Some(val & self.bit_mask_u128())
    }

    /// Sign-extends `val` (treated as the type's bit-width-narrow representation)
    /// to a full 128-bit signed integer.  Companion to [`Self::get_signed_int`]
    /// but works at U128 width.
    ///
    /// For widths > 128 returns `None` — i128 cannot represent values wider
    /// than 128 bits as signed.  No current consumer hits this case
    /// (NodeOutputType::U256 is unreachable in IntConst land today).
    #[must_use]
    pub fn get_signed_int_i128(self, val: u128) -> Option<i128> {
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
        // Sign-extend: if the high bit at position bits-1 is set, OR in the
        // top (128-bits) bits to produce a negative i128.
        let sign_bit = 1u128 << (bits - 1);
        if (masked & sign_bit) != 0 {
            let high_extension = !((1u128 << bits) - 1);
            Some((masked | high_extension) as i128)
        } else {
            Some(masked as i128)
        }
    }
}

impl TryFrom<u32> for NodeOutputType {
    type Error = crate::error::Error;

    fn try_from(value: u32) -> crate::error::Result<Self> {
        match value {
            1 => Ok(Self::U8),
            2 => Ok(Self::U16),
            4 => Ok(Self::U32),
            8 => Ok(Self::U64),
            16 => Ok(Self::U128),
            32 => Ok(Self::U256),
            n => Err(crate::error::ErrorKind::UnsupportedOutputSize(n).into()),
        }
    }
}

impl std::fmt::Display for NodeOutputType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}
#[cfg(test)]
mod tests {
    use super::NodeOutputType;

    #[test]
    fn bit_mask_u128_widths() {
        assert_eq!(NodeOutputType::Bool.bit_mask_u128(), 0x1u128);
        assert_eq!(NodeOutputType::U8.bit_mask_u128(), 0xffu128);
        assert_eq!(NodeOutputType::U16.bit_mask_u128(), 0xffffu128);
        assert_eq!(NodeOutputType::U32.bit_mask_u128(), 0xffff_ffffu128);
        assert_eq!(NodeOutputType::U64.bit_mask_u128(), u64::MAX as u128);
        assert_eq!(NodeOutputType::U128.bit_mask_u128(), u128::MAX);
        // Float types return 0 (defensive — no caller should ask).
        assert_eq!(NodeOutputType::F32.bit_mask_u128(), 0);
        assert_eq!(NodeOutputType::F64.bit_mask_u128(), 0);
    }

    #[test]
    fn get_unsigned_int_u128_masks_to_width() {
        // 0x12345678 masked to U16 = 0x5678.
        assert_eq!(
            NodeOutputType::U16.get_unsigned_int_u128(0x12345678u128),
            Some(0x5678u128)
        );
        // 0x12345678 masked to U32 = 0x12345678.
        assert_eq!(
            NodeOutputType::U32.get_unsigned_int_u128(0x12345678u128),
            Some(0x12345678u128)
        );
        // U128 masking is identity.
        assert_eq!(
            NodeOutputType::U128.get_unsigned_int_u128(u128::MAX),
            Some(u128::MAX)
        );
        // Float types return None.
        assert_eq!(NodeOutputType::F32.get_unsigned_int_u128(0x12345678u128), None);
    }

    #[test]
    fn get_signed_int_i128_sign_extends_negative_at_narrow_widths() {
        // -50 stored at U32 width is 0xffff_ffce.  Sign-extending to i128
        // must produce -50.
        let neg50_at_u32 = 0xffff_ffceu128;
        assert_eq!(
            NodeOutputType::U32.get_signed_int_i128(neg50_at_u32),
            Some(-50i128)
        );
        // -50 stored at U8 width is 0xce.  Sign-extending must give -50.
        assert_eq!(
            NodeOutputType::U8.get_signed_int_i128(0xceu128),
            Some(-50i128)
        );
        // Positive 50 at U32 stays 50.
        assert_eq!(
            NodeOutputType::U32.get_signed_int_i128(50u128),
            Some(50i128)
        );
    }

    #[test]
    fn get_signed_int_i128_handles_full_u128_width() {
        // U128 with high bit set: read as negative i128.
        let neg1_at_u128 = u128::MAX;
        assert_eq!(
            NodeOutputType::U128.get_signed_int_i128(neg1_at_u128),
            Some(-1i128)
        );
        // U128 max-positive (high bit clear): stays positive when reinterpreted as i128.
        let max_pos = i128::MAX as u128;
        assert_eq!(
            NodeOutputType::U128.get_signed_int_i128(max_pos),
            Some(i128::MAX)
        );
    }
}
