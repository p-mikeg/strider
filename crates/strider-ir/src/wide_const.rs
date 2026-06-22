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
            Self::I256(_) => 32,
            Self::I512(_) => 64,
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

    /// The value as a `u64` if every bit above bit 63 is zero, else `None`.
    ///
    /// Lets construction store a small-valued wide constant inline as
    /// `IntPayload::Small` (the declared output type carries the width) so
    /// the interner is reserved for values that genuinely exceed `u64`.
    pub fn as_u64(&self) -> Option<u64> {
        match self {
            Self::I80(v) | Self::I128(v) => u64::try_from(*v).ok(),
            Self::I256(_) | Self::I512(_) => {
                self.limbs()[1..].iter().all(|&l| l == 0).then_some(self.limbs()[0])
            }
        }
    }

    /// Returns the storage as a contiguous little-endian byte vector.
    /// Backs [`crate::IRViewer::int_const_wide_le_bytes`], which the
    /// strider-py `wide_const_bytes` / `const_int` accessors use to
    /// surface wide constants to Python.
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

    /// Interning the same value twice returns the same id, for every
    /// storage width.
    #[test]
    fn intern_dedups_equal_values_per_width() {
        // Rows: (label = former test name, storage value).
        let cases: [(&str, WideConstStorage); 4] = [
            (
                "intern_dedups_equal_i80_values",
                WideConstStorage::I80(0xABCD),
            ),
            (
                "intern_dedups_equal_i128_values",
                WideConstStorage::I128(1u128 << 100),
            ),
            (
                "intern_dedups_equal_u256_values",
                WideConstStorage::I256([1, 2, 3, 4]),
            ),
            (
                "intern_dedups_equal_u512_values",
                WideConstStorage::I512([0xdead; 8]),
            ),
        ];
        for (label, v) in cases {
            let mut g = Function::default();
            let id1 = g.intern_wide_const(v.clone());
            let id2 = g.intern_wide_const(v);
            assert_eq!(
                id1, id2,
                "{label}: interning the same value must return the same id"
            );
        }
    }

    /// Distinct values — including same-numeric-value pairs at different
    /// widths — get distinct ids.
    #[test]
    fn intern_assigns_distinct_ids_for_distinct_values() {
        // Rows: (label = former test name, first storage, second storage).
        let cases: [(&str, WideConstStorage, WideConstStorage); 3] = [
            (
                "intern_assigns_distinct_ids_for_distinct_values",
                WideConstStorage::I256([1; 4]),
                WideConstStorage::I256([2; 4]),
            ),
            (
                "intern_distinguishes_u256_from_u512_with_same_low_limbs",
                WideConstStorage::I256([1, 0, 0, 0]),
                WideConstStorage::I512([1, 0, 0, 0, 0, 0, 0, 0]),
            ),
            (
                "intern_distinguishes_i80_from_i128_with_same_value",
                WideConstStorage::I80(42),
                WideConstStorage::I128(42),
            ),
        ];
        for (label, a, b) in cases {
            let mut g = Function::default();
            let id_a = g.intern_wide_const(a);
            let id_b = g.intern_wide_const(b);
            assert_ne!(id_a, id_b, "{label}: distinct values must get distinct ids");
        }
    }

    #[test]
    fn wide_const_lookup_returns_stored_value() {
        let mut g = Function::default();
        let v = WideConstStorage::I256([0x1234, 0x5678, 0x9abc, 0xdef0]);
        let id = g.intern_wide_const(v.clone());
        assert_eq!(g.wide_const(id), &v);
    }

    /// `byte_size` reports the storage width: 10 (I80), 16 (I128),
    /// 32 (I256), 64 (I512).
    #[test]
    fn byte_size_per_width() {
        // Rows: (label = former test name, storage, expected byte size).
        let cases: [(&str, WideConstStorage, usize); 4] = [
            ("i80_byte_size_is_10", WideConstStorage::I80(0), 10),
            ("i128_byte_size_is_16", WideConstStorage::I128(0), 16),
            ("u256_byte_size_is_32", WideConstStorage::I256([0; 4]), 32),
            ("u512_byte_size_is_64", WideConstStorage::I512([0; 8]), 64),
        ];
        for (label, v, expected) in cases {
            assert_eq!(v.byte_size(), expected, "{label}");
        }
    }

    #[test]
    fn as_u128_returns_some_for_i80_and_i128() {
        let v = 0xDEAD_BEEFu128;
        assert_eq!(WideConstStorage::I80(v).as_u128(), Some(v));
        assert_eq!(WideConstStorage::I128(v).as_u128(), Some(v));
        assert_eq!(WideConstStorage::I256([0; 4]).as_u128(), None);
        assert_eq!(WideConstStorage::I512([0; 8]).as_u128(), None);
    }

    /// `to_le_bytes` serialises each width to its full byte length in
    /// little-endian order (low byte first, zero-padded to the top).
    #[test]
    fn to_le_bytes_serialises_little_endian_per_width() {
        // The low limb is 0x0807_0605_0403_0201 — bytes 1..=8 LE — and (for
        // I512) the second limb continues 9..=16.
        let low: u128 = 0x0807_0605_0403_0201;
        let mut expected_512: Vec<u8> = (1u8..=16).collect();
        expected_512.extend(std::iter::repeat_n(0u8, 48));
        // Rows: (label = former test name, storage, expected full LE bytes).
        let cases: [(&str, WideConstStorage, Vec<u8>); 4] = [
            (
                "to_le_bytes_i80_is_10_bytes_little_endian",
                WideConstStorage::I80(low),
                vec![1, 2, 3, 4, 5, 6, 7, 8, 0, 0],
            ),
            (
                "to_le_bytes_i128_is_16_bytes_little_endian",
                WideConstStorage::I128(low),
                vec![1, 2, 3, 4, 5, 6, 7, 8, 0, 0, 0, 0, 0, 0, 0, 0],
            ),
            (
                "to_le_bytes_u256_serialises_little_endian",
                WideConstStorage::I256([low as u64, 0, 0, 0]),
                {
                    let mut v: Vec<u8> = (1u8..=8).collect();
                    v.extend(std::iter::repeat_n(0u8, 24));
                    v
                },
            ),
            (
                "to_le_bytes_u512_serialises_little_endian",
                WideConstStorage::I512([low as u64, 0x100f_0e0d_0c0b_0a09, 0, 0, 0, 0, 0, 0]),
                expected_512,
            ),
        ];
        for (label, v, expected) in cases {
            assert_eq!(v.to_le_bytes(), expected, "{label}");
        }
    }
}
