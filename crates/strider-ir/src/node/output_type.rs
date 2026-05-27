//! Concrete value types carried by node outputs.

/// The value type carried by a node output.
///
/// Integer variants correspond directly to their C-style unsigned integer
/// widths.  `Bool` is a 1-bit logical value.  `F32`/`F64` are IEEE 754
/// floating-point types whose raw bit patterns are stored as `u64`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum NodeOutputType {
    Bool,
    I8,
    I16,
    I32,
    I64,
    /// 80-bit unsigned integer.  Models x87 ST0/STn registers'
    /// integer/bit-pattern view; values are stored in `u128` payloads
    /// masked to the low 80 bits.  No native Rust type matches this
    /// width, so opt rules that need a `u64`-fitting value return
    /// `None` for I80 and let the rule skip.
    I80,
    I128,
    I256,
    /// 512-bit unsigned integer (AVX-512 `zmm` registers).  Stored
    /// off-side via [`crate::wide_const::WideConstStorage::I512`]
    /// because the value doesn't fit in `IntConst`'s `u128` payload.
    I512,
    /// 32-bit IEEE 754 single-precision float.
    F32,
    /// 64-bit IEEE 754 double-precision float.
    F64,
    /// 80-bit x87 extended-precision float (Intel "long double" /
    /// 80-bit FPU stack registers).  Rust has no native `f80`, so opt
    /// rules don't constant-fold F80 arithmetic — the nodes simply
    /// remain in the IR for pattern-matching workloads.  Bit-conversion
    /// constructors (`IntBitsToFloat` / `FloatBitsToInt`) skip the
    /// immediate-fold for F80 because `FloatConst`'s u64 payload can't
    /// hold the 80-bit pattern.
    F80,
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
    TypeInfo { name: "i8",   byte_size: 1,  category: NodeOutputTypeCategory::Int   },
    TypeInfo { name: "i16",  byte_size: 2,  category: NodeOutputTypeCategory::Int   },
    TypeInfo { name: "i32",  byte_size: 4,  category: NodeOutputTypeCategory::Int   },
    TypeInfo { name: "i64",  byte_size: 8,  category: NodeOutputTypeCategory::Int   },
    TypeInfo { name: "i80",  byte_size: 10, category: NodeOutputTypeCategory::Int   },
    TypeInfo { name: "i128", byte_size: 16, category: NodeOutputTypeCategory::Int   },
    TypeInfo { name: "i256", byte_size: 32, category: NodeOutputTypeCategory::Int   },
    TypeInfo { name: "i512", byte_size: 64, category: NodeOutputTypeCategory::Int   },
    TypeInfo { name: "f32",  byte_size: 4,  category: NodeOutputTypeCategory::Float },
    TypeInfo { name: "f64",  byte_size: 8,  category: NodeOutputTypeCategory::Float },
    TypeInfo { name: "f80",  byte_size: 10, category: NodeOutputTypeCategory::Float },
];

impl NodeOutputType {
    /// Returns the type's [`TypeInfo`] entry.  Implemented as an
    /// exhaustive `match` rather than `&TYPE_INFO[self as usize]` so
    /// adding a new variant is a compile-time error rather than a
    /// runtime out-of-bounds index.  The `TYPE_INFO` table itself is
    /// validated against the enum order by the
    /// `type_info_table_matches_variants` test.
    #[inline]
    fn info(self) -> &'static TypeInfo {
        match self {
            Self::Bool => &TYPE_INFO[0],
            Self::I8 => &TYPE_INFO[1],
            Self::I16 => &TYPE_INFO[2],
            Self::I32 => &TYPE_INFO[3],
            Self::I64 => &TYPE_INFO[4],
            Self::I80 => &TYPE_INFO[5],
            Self::I128 => &TYPE_INFO[6],
            Self::I256 => &TYPE_INFO[7],
            Self::I512 => &TYPE_INFO[8],
            Self::F32 => &TYPE_INFO[9],
            Self::F64 => &TYPE_INFO[10],
            Self::F80 => &TYPE_INFO[11],
        }
    }

    /// Returns the canonical name of this type as a static string.
    #[inline]
    #[must_use]
    pub fn as_str(self) -> &'static str {
        self.info().name
    }

    /// Returns the size of this type **in bytes**.
    ///
    /// Both `Bool` and `I8` return 1.
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
    /// Returns `true` for `Bool`, `I8`, `I16`, `I32`, `I64`, `F32`, and `F64`.
    /// Returns `false` for `I80` (10 bytes), `I128`, `I256`, `I512`, and `F80`
    /// (10 bytes).
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

    /// Returns `true` if this type is one of the unsigned integer
    /// variants (I8, I16, I32, I64, I80, I128, I256, I512).
    #[inline]
    #[must_use]
    pub fn is_integer(self) -> bool {
        matches!(self.info().category, NodeOutputTypeCategory::Int)
    }

    /// Returns `true` if this type is one of the float variants
    /// (F32, F64, F80).
    #[inline]
    #[must_use]
    pub fn is_float(self) -> bool {
        matches!(self.info().category, NodeOutputTypeCategory::Float)
    }

    /// Returns the unsigned integer type with the same byte size.
    /// (Bool→I8, F32→I32, F64→I64, Ux→Ux)
    #[inline]
    #[must_use]
    pub fn to_natural_int_type(self) -> NodeOutputType {
        match self {
            NodeOutputType::Bool | NodeOutputType::I8 => NodeOutputType::I8,
            NodeOutputType::I16 => NodeOutputType::I16,
            NodeOutputType::I32 | NodeOutputType::F32 => NodeOutputType::I32,
            NodeOutputType::I64 | NodeOutputType::F64 => NodeOutputType::I64,
            NodeOutputType::I80 | NodeOutputType::F80 => NodeOutputType::I80,
            NodeOutputType::I128 => NodeOutputType::I128,
            NodeOutputType::I256 => NodeOutputType::I256,
            NodeOutputType::I512 => NodeOutputType::I512,
        }
    }

    /// Returns the all-ones bit mask for this integer type, as `u128`.
    /// `Bool` returns `1`; integer widths up to 128 bits return their
    /// natural bit widths (e.g. `I64` returns `0xFFFF_FFFF_FFFF_FFFF`).
    /// `I128` returns `u128::MAX`.  `I256` and `I512` also return
    /// `u128::MAX` because the mask cannot represent 256+ bits in a
    /// `u128` carrier — callers that need to mask a 256-bit value must
    /// route through `IntConstWide` / `Graph::wide_consts`.  Wide-type
    /// rejection happens at the `IntConst` build site
    /// ([`crate::FunctionBuilder::build_int_const`] returns `Err`
    /// for `I256` / `I512`); `bit_mask_u128` and
    /// [`Self::get_unsigned_int`] do not reject `I256` themselves —
    /// they return the conservative `u128`-width approximation.
    /// Float types return `0` (defensive — no caller should ask).
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

    /// Masks `val` to this type's bit width and returns the result, or `None`
    /// if this type is not an integer (`Bool`, `F32`, `F64`, `F80`).
    ///
    /// For widths >= 128 returns `val` unchanged (the carrier is `u128`, so
    /// `I128` returns its full mask and `I256` returns `val` as-is - callers
    /// that need to distinguish the two must check the type explicitly).
    ///
    /// **Bool exception.**  `Bool` is excluded even though
    /// [`Self::bit_mask_u128`] returns `1` for it: `Bool` is its own
    /// category in [`crate::node::NodeKind`], so callers reading a `Bool` constant
    /// should match the `BoolConst` arm directly rather than going
    /// through this helper.
    #[must_use]
    pub fn get_unsigned_int(self, val: u128) -> Option<u128> {
        if !self.is_integer() {
            return None;
        }
        Some(val & self.bit_mask_u128())
    }

    /// Sign-extends `val` (treated as the type's bit-width-narrow
    /// representation) to a full 128-bit signed integer, or returns `None`
    /// if this type is not an integer or its width exceeds 128 bits
    /// (`I256` — unreachable in `IntConst` land today).
    #[must_use]
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

impl NodeOutputType {
    /// Maps a varnode byte size to the corresponding **integer** output
    /// type: `1 → I8`, `2 → I16`, `4 → I32`, `8 → I64`, `10 → I80`,
    /// `16 → I128`, `32 → I256`, `64 → I512`.
    ///
    /// Byte size 1 maps to `I8`, never `I1` — `I1` (the 1-bit boolean) is
    /// produced only by comparisons and logical ops, not by a varnode width.
    ///
    /// # Errors
    ///
    /// Returns an error for any byte size that has no corresponding integer
    /// type.
    pub fn int_for_byte_size(n: u32) -> crate::error::Result<Self> {
        match n {
            1 => Ok(Self::I8),
            2 => Ok(Self::I16),
            4 => Ok(Self::I32),
            8 => Ok(Self::I64),
            10 => Ok(Self::I80),
            16 => Ok(Self::I128),
            32 => Ok(Self::I256),
            64 => Ok(Self::I512),
            n => Err(anyhow::anyhow!("unsupported node output size: {n} bytes")),
        }
    }

    /// Maps a varnode byte size to the corresponding **float** output type:
    /// `4 → F32`, `8 → F64`, `10 → F80` (x87 extended precision).
    ///
    /// Mirrors `TryFrom<u32>` for the integer side; kept as a dedicated
    /// helper because the float subset is open to fewer sizes and the
    /// caller's error message references "float varnode size".
    ///
    /// # Errors
    ///
    /// Returns an error for any byte size other than 4, 8, or 10.
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
        assert_eq!(NodeOutputType::I8.bit_mask_u128(), 0xffu128);
        assert_eq!(NodeOutputType::I16.bit_mask_u128(), 0xffffu128);
        assert_eq!(NodeOutputType::I32.bit_mask_u128(), 0xffff_ffffu128);
        assert_eq!(NodeOutputType::I64.bit_mask_u128(), u64::MAX as u128);
        assert_eq!(NodeOutputType::I128.bit_mask_u128(), u128::MAX);
        // Float types return 0 (defensive — no caller should ask).
        assert_eq!(NodeOutputType::F32.bit_mask_u128(), 0);
        assert_eq!(NodeOutputType::F64.bit_mask_u128(), 0);
    }

    #[test]
    fn get_unsigned_int_masks_to_width() {
        assert_eq!(
            NodeOutputType::I16.get_unsigned_int(0x12345678u128),
            Some(0x5678u128)
        );
        assert_eq!(
            NodeOutputType::I32.get_unsigned_int(0x12345678u128),
            Some(0x12345678u128)
        );
        assert_eq!(
            NodeOutputType::I128.get_unsigned_int(u128::MAX),
            Some(u128::MAX)
        );
        assert_eq!(NodeOutputType::F32.get_unsigned_int(0x12345678u128), None);
        assert_eq!(NodeOutputType::Bool.get_unsigned_int(1), None);
    }

    #[test]
    fn get_signed_int_sign_extends_negative_at_narrow_widths() {
        let neg50_at_u32 = 0xffff_ffceu128;
        assert_eq!(
            NodeOutputType::I32.get_signed_int(neg50_at_u32),
            Some(-50i128)
        );
        assert_eq!(
            NodeOutputType::I8.get_signed_int(0xceu128),
            Some(-50i128)
        );
        assert_eq!(
            NodeOutputType::I32.get_signed_int(50u128),
            Some(50i128)
        );
    }

    #[test]
    fn get_signed_int_handles_full_u128_width() {
        let neg1_at_u128 = u128::MAX;
        assert_eq!(
            NodeOutputType::I128.get_signed_int(neg1_at_u128),
            Some(-1i128)
        );
        let max_pos = i128::MAX as u128;
        assert_eq!(
            NodeOutputType::I128.get_signed_int(max_pos),
            Some(i128::MAX)
        );
    }

    // ── F80 / I80 (x87 80-bit FPU) ────────────────────────────────────────

    /// `I80` and `F80` widths must be 10 bytes / 80 bits — the x87 ST0
    /// register width that the lifter needs in order to handle x86
    /// floats without erroring at `analyze_cfg` setup.
    #[test]
    fn u80_f80_widths() {
        assert_eq!(NodeOutputType::I80.byte_size(), 10);
        assert_eq!(NodeOutputType::I80.bit_width(), 80);
        assert_eq!(NodeOutputType::F80.byte_size(), 10);
        assert_eq!(NodeOutputType::F80.bit_width(), 80);
    }

    /// `I80` is an integer type; `F80` is a float type.  The category
    /// classifier drives `is_integer` / `is_float`, used by validator
    /// signature checks (the local-typing check) and by the lifter's coerce helpers.
    #[test]
    fn u80_is_integer_and_f80_is_float() {
        assert!(NodeOutputType::I80.is_integer());
        assert!(!NodeOutputType::I80.is_float());
        assert!(NodeOutputType::F80.is_float());
        assert!(!NodeOutputType::F80.is_integer());
    }

    /// `to_natural_int_type` must map `F80 → I80` (mirrors `F64 → I64`)
    /// and `I80 → I80` (identity).  This is the path the lifter's
    /// `read_reg_vn` / `write_reg_vn` use when bridging between float
    /// and integer views of the same SSA variable.
    #[test]
    fn to_natural_int_type_handles_u80_and_f80() {
        assert_eq!(
            NodeOutputType::I80.to_natural_int_type(),
            NodeOutputType::I80
        );
        assert_eq!(
            NodeOutputType::F80.to_natural_int_type(),
            NodeOutputType::I80
        );
    }

    /// `bit_mask_u128(I80)` must be `(1u128 << 80) - 1` — the 80-bit
    /// all-ones mask.  Existing opt rules use this mask for value-aware
    /// rewrites like `x & all_ones → x`.
    #[test]
    fn bit_mask_u128_for_u80() {
        let expected: u128 = (1u128 << 80) - 1;
        assert_eq!(NodeOutputType::I80.bit_mask_u128(), expected);
        // F80 is a float type — defensive `0` like F32/F64.
        assert_eq!(NodeOutputType::F80.bit_mask_u128(), 0);
    }

    #[test]
    fn get_unsigned_int_for_u80_masks_to_80_bits() {
        let raw: u128 = 0xFFFF_FFFF_FFFF_FFFF_FFFF_FFFF_FFFF_FFFFu128;
        let mask: u128 = (1u128 << 80) - 1;
        assert_eq!(NodeOutputType::I80.get_unsigned_int(raw), Some(mask));
        assert_eq!(
            NodeOutputType::I80.get_unsigned_int(0x12345678),
            Some(0x12345678)
        );
    }

    #[test]
    fn get_signed_int_for_u80_sign_extends() {
        let neg1_at_u80 = (1u128 << 80) - 1;
        assert_eq!(
            NodeOutputType::I80.get_signed_int(neg1_at_u80),
            Some(-1i128)
        );
        assert_eq!(NodeOutputType::I80.get_signed_int(50u128), Some(50i128));
        let neg50 = ((1u128 << 80) - 1) ^ 49;
        assert_eq!(
            NodeOutputType::I80.get_signed_int(neg50),
            Some(-50i128)
        );
    }

    /// `int_for_byte_size` must accept 10 → I80 so the lifter's
    /// varnode→type conversion succeeds for x87 80-bit regs.
    #[test]
    fn int_for_byte_size_10_is_i80() {
        let ty = NodeOutputType::int_for_byte_size(10).expect("10 must convert to I80");
        assert_eq!(ty, NodeOutputType::I80);
    }

    /// `int_for_byte_size` maps every supported width and rejects others.
    #[test]
    fn int_for_byte_size_maps_widths() {
        use super::NodeOutputType as T;
        assert_eq!(T::int_for_byte_size(1).unwrap(), T::I8);
        assert_eq!(T::int_for_byte_size(8).unwrap(), T::I64);
        assert_eq!(T::int_for_byte_size(64).unwrap(), T::I512);
        assert!(T::int_for_byte_size(3).is_err());
    }

    /// `bit_mask_u128` for `I256` and `I512` must return
    /// `u128::MAX` — the conservative `u128`-width approximation, since
    /// these widths exceed the carrier.  Pins the `bits >= 128` guard.
    #[test]
    fn bit_mask_u128_for_u256_and_u512_is_u128_max() {
        assert_eq!(NodeOutputType::I256.bit_mask_u128(), u128::MAX);
        assert_eq!(NodeOutputType::I512.bit_mask_u128(), u128::MAX);
    }

    /// `get_unsigned_int` for `I256`/`I512` passes through
    /// values within the `u128` carrier (no false rejection).
    #[test]
    fn get_unsigned_int_for_u256_passes_through_small_values() {
        assert_eq!(
            NodeOutputType::I256.get_unsigned_int(0xDEAD_BEEFu128),
            Some(0xDEAD_BEEFu128),
        );
        assert_eq!(NodeOutputType::I256.get_unsigned_int(u128::MAX), Some(u128::MAX));
        assert_eq!(NodeOutputType::I512.get_unsigned_int(42u128), Some(42u128));
    }
}
