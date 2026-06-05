//! Wide-integer constant storage — values whose storage benefits from the
//! interner rather than inline `IntConst(Small(u64))` payload.
//!
//! [`crate::node::NodeKind::IntConst`] has two forms:
//! - `IntConst(IntPayload::Small(v))` inlines ≤ 64-bit values (I1..I64).
//! - `IntConst(IntPayload::Wide(id))` references wider integer types
//!   (I80, I128, I256, I512) stored in `crate::Function::wide_const_interner`.
//!
//! ## Interning contract
//!
//! `crate::Function::intern_wide_const` dedups by value: two interns of
//! the same [`WideConstStorage`] always return the same id.  This is
//! load-bearing for the dedup-cache contract — two structurally
//! identical `IntConst(Wide(id))` nodes (same id) are equal at the
//! `(kind, inputs, output_kinds)` cache-key level, so [`crate::Graph::create_node`]
//! sees them as equal and reuses the same `NodeId`.  Without value-level
//! interning the cache would key on graph-local ids that two callers
//! might have allocated independently for the same logical value.

use cranelift_entity::entity_impl;

/// Dense, typed identifier for a wide-integer constant value stored in
/// `crate::Function::wide_const_interner`.
///
/// Allocated by `crate::Function::intern_wide_const`; opaque to
/// callers — use [`crate::Function::wide_const`] to look up the value.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct WideConstId(u32);
entity_impl!(WideConstId, "wide_const");

/// Storage for a wide-integer constant value — the byte payload the IR
/// carries when an `IntConst(IntPayload::Wide(id))` node
/// produces an I80, I128, I256, or I512 output.
///
/// `I80` and `I128` store their value directly as a `u128` (the low 80
/// or 128 bits are significant).  `I256` and `I512` use little-endian
/// limb arrays: `limbs[0]` is the low 64 bits, `limbs[N-1]` the high.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum WideConstStorage {
    /// 80-bit (x87 extended) value; the low 80 bits of the `u128` are
    /// significant, the top 48 bits are always zero.
    I80(u128),
    /// 128-bit value.
    I128(u128),
    /// 256-bit unsigned integer (AVX-2 `ymm`).
    I256([u64; 4]),
    /// 512-bit unsigned integer (AVX-512 `zmm`).
    I512([u64; 8]),
}

impl WideConstStorage {
    /// Returns the byte width of this storage (10, 16, 32, or 64).
    pub fn byte_size(&self) -> usize {
        match self {
            Self::I80(_) => 10,
            Self::I128(_) => 16,
            Self::I256(limbs) => limbs.len() * 8,
            Self::I512(limbs) => limbs.len() * 8,
        }
    }

    /// Returns the limb storage as a slice — `limbs[0]` is the low
    /// 64 bits, `limbs[N-1]` the high.  Length is 4 for `I256`, 8 for
    /// `I512`.
    ///
    /// # Panics
    ///
    /// Panics for `I80` / `I128` — those variants carry a `u128` value
    /// directly rather than a limb array.  Callers that operate on all
    /// variants must match explicitly and use [`Self::as_u128`] for the
    /// narrow variants.
    pub fn limbs(&self) -> &[u64] {
        match self {
            Self::I256(limbs) => limbs,
            Self::I512(limbs) => limbs,
            Self::I80(_) | Self::I128(_) => {
                panic!("WideConstStorage::limbs() called on I80/I128 variant (no limb array)")
            }
        }
    }

    /// The value as a `u128` if it fits (I80/I128), else `None` (I256/I512).
    pub fn as_u128(&self) -> Option<u128> {
        match self {
            Self::I80(v) | Self::I128(v) => Some(*v),
            _ => None,
        }
    }

    /// Returns the storage as a contiguous little-endian byte vector.
    /// Used by the pattern crate's `Match::get_wide_bytes` accessor and
    /// by the strider-py wrapper to surface the raw bytes to Python.
    pub fn to_le_bytes(&self) -> Vec<u8> {
        match self {
            Self::I80(v) => {
                // 10 bytes: low 80 bits of the u128, little-endian.
                let all = v.to_le_bytes(); // 16 bytes
                all[..10].to_vec()
            }
            Self::I128(v) => v.to_le_bytes().to_vec(),
            Self::I256(_) | Self::I512(_) => {
                self.limbs().iter().flat_map(|l| l.to_le_bytes()).collect()
            }
        }
    }

}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Function;

    #[test]
    fn intern_dedups_equal_u256_values() {
        let mut g = Function::default();
        let v = WideConstStorage::I256([1, 2, 3, 4]);
        let id1 = g.intern_wide_const(v.clone());
        let id2 = g.intern_wide_const(v);
        assert_eq!(id1, id2, "interning the same value must return the same id");
    }

    #[test]
    fn intern_dedups_equal_u512_values() {
        let mut g = Function::default();
        let v = WideConstStorage::I512([0xdead; 8]);
        let id1 = g.intern_wide_const(v.clone());
        let id2 = g.intern_wide_const(v);
        assert_eq!(id1, id2);
    }

    #[test]
    fn intern_assigns_distinct_ids_for_distinct_values() {
        let mut g = Function::default();
        let id1 = g.intern_wide_const(WideConstStorage::I256([1; 4]));
        let id2 = g.intern_wide_const(WideConstStorage::I256([2; 4]));
        assert_ne!(id1, id2);
    }

    #[test]
    fn intern_distinguishes_u256_from_u512_with_same_low_limbs() {
        let mut g = Function::default();
        let id_256 = g.intern_wide_const(WideConstStorage::I256([1, 0, 0, 0]));
        let id_512 = g.intern_wide_const(WideConstStorage::I512([1, 0, 0, 0, 0, 0, 0, 0]));
        assert_ne!(id_256, id_512);
    }

    #[test]
    fn intern_distinguishes_i80_from_i128_with_same_value() {
        let mut g = Function::default();
        let id_80 = g.intern_wide_const(WideConstStorage::I80(42));
        let id_128 = g.intern_wide_const(WideConstStorage::I128(42));
        assert_ne!(id_80, id_128, "I80 and I128 with same value must get distinct ids");
    }

    #[test]
    fn intern_dedups_equal_i80_values() {
        let mut g = Function::default();
        let id1 = g.intern_wide_const(WideConstStorage::I80(0xABCD));
        let id2 = g.intern_wide_const(WideConstStorage::I80(0xABCD));
        assert_eq!(id1, id2);
    }

    #[test]
    fn intern_dedups_equal_i128_values() {
        let mut g = Function::default();
        let big = 1u128 << 100;
        let id1 = g.intern_wide_const(WideConstStorage::I128(big));
        let id2 = g.intern_wide_const(WideConstStorage::I128(big));
        assert_eq!(id1, id2);
    }

    #[test]
    fn wide_const_lookup_returns_stored_value() {
        let mut g = Function::default();
        let v = WideConstStorage::I256([0x1234, 0x5678, 0x9abc, 0xdef0]);
        let id = g.intern_wide_const(v.clone());
        assert_eq!(g.wide_const(id), &v);
    }

    #[test]
    fn u256_byte_size_is_32() {
        assert_eq!(WideConstStorage::I256([0; 4]).byte_size(), 32);
    }

    #[test]
    fn u512_byte_size_is_64() {
        assert_eq!(WideConstStorage::I512([0; 8]).byte_size(), 64);
    }

    #[test]
    fn i80_byte_size_is_10() {
        assert_eq!(WideConstStorage::I80(0).byte_size(), 10);
    }

    #[test]
    fn i128_byte_size_is_16() {
        assert_eq!(WideConstStorage::I128(0).byte_size(), 16);
    }

    #[test]
    fn as_u128_returns_some_for_i80_and_i128() {
        let v = 0xDEAD_BEEFu128;
        assert_eq!(WideConstStorage::I80(v).as_u128(), Some(v));
        assert_eq!(WideConstStorage::I128(v).as_u128(), Some(v));
        assert_eq!(WideConstStorage::I256([0; 4]).as_u128(), None);
        assert_eq!(WideConstStorage::I512([0; 8]).as_u128(), None);
    }

    #[test]
    fn to_le_bytes_i80_is_10_bytes_little_endian() {
        // Value 0x0807_0605_0403_0201 fits in 8 bytes; bytes 8-9 are 0.
        let v: u128 = 0x0807_0605_0403_0201;
        let bytes = WideConstStorage::I80(v).to_le_bytes();
        assert_eq!(bytes.len(), 10);
        assert_eq!(&bytes[..8], &[1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(&bytes[8..], &[0, 0]);
    }

    #[test]
    fn to_le_bytes_i128_is_16_bytes_little_endian() {
        let v: u128 = 0x0807_0605_0403_0201;
        let bytes = WideConstStorage::I128(v).to_le_bytes();
        assert_eq!(bytes.len(), 16);
        assert_eq!(&bytes[..8], &[1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(&bytes[8..], &[0u8; 8]);
    }

    #[test]
    fn to_le_bytes_u256_serialises_little_endian() {
        let v = WideConstStorage::I256([0x0807_0605_0403_0201, 0, 0, 0]);
        let bytes = v.to_le_bytes();
        assert_eq!(&bytes[..8], &[1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(bytes.len(), 32);
    }

    #[test]
    fn to_le_bytes_u512_serialises_little_endian() {
        let v = WideConstStorage::I512([
            0x0807_0605_0403_0201,
            0x100f_0e0d_0c0b_0a09,
            0,
            0,
            0,
            0,
            0,
            0,
        ]);
        let bytes = v.to_le_bytes();
        assert_eq!(&bytes[..16], &(1u8..=16).collect::<Vec<u8>>()[..]);
        assert_eq!(bytes.len(), 64);
    }

}
