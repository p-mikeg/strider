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
