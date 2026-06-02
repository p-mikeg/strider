//! Wide-integer constant storage — values whose width exceeds `u128`.
//!
//! [`crate::node::NodeKind::IntConst`] inlines values up to 128 bits in
//! its payload word.  The wider integer types (`I256` for AVX-2 ymm
//! registers, `I512` for AVX-512 zmm) don't fit; they live in
//! `Graph::wide_const_interner` and are referenced from the IR via
//! [`crate::node::NodeKind::IntConstWide`] carrying a [`WideConstId`].
//!
//! ## Interning contract
//!
//! `crate::Graph::intern_wide_const` dedups by value: two interns of
//! the same [`WideConstStorage`] always return the same id.  This is
//! load-bearing for the dedup-cache contract — two structurally
//! identical `IntConstWide(id)` nodes (same id) are equal at the
//! `(kind, inputs, output_kinds)` cache-key level, so [`crate::Graph::create_node`]
//! sees them as equal and reuses the same `NodeId`.  Without value-level
//! interning the cache would key on graph-local ids that two callers
//! might have allocated independently for the same logical value.

use cranelift_entity::entity_impl;

/// Dense, typed identifier for a wide-integer constant value stored in
/// `Graph::wide_const_interner`.
///
/// Allocated by `crate::Graph::intern_wide_const`; opaque to
/// callers — use [`crate::Graph::wide_const`] to look up the value.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct WideConstId(u32);
entity_impl!(WideConstId, "wide_const");

/// Storage for a wide-integer constant value — the byte payload the IR
/// carries when an [`crate::node::NodeKind::IntConstWide`] node
/// produces a `I256` or `I512` output.
///
/// Limbs are little-endian: `limbs[0]` is the low 64 bits, `limbs[N-1]`
/// the high 64 bits.  This matches the lifter's natural ordering when
/// it slices a wide register read into u64 pieces.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum WideConstStorage {
    /// 256-bit unsigned integer (AVX-2 `ymm`).
    I256([u64; 4]),
    /// 512-bit unsigned integer (AVX-512 `zmm`).
    I512([u64; 8]),
}

impl WideConstStorage {
    /// Returns the byte width of this storage (32 for I256, 64 for I512).
    #[must_use]
    pub fn byte_size(&self) -> usize {
        self.limbs().len() * 8
    }

    /// Returns the limb storage as a slice — `limbs[0]` is the low
    /// 64 bits, `limbs[N-1]` the high.  Length is 4 for `I256`, 8 for
    /// `I512`.  Projection over the variant difference so width-agnostic
    /// callers (`byte_size`, `to_le_bytes`) don't have to match.
    #[must_use]
    pub fn limbs(&self) -> &[u64] {
        match self {
            Self::I256(limbs) => limbs,
            Self::I512(limbs) => limbs,
        }
    }

    /// Returns the storage as a contiguous little-endian byte vector.
    /// Used by the pattern crate's `Match::get_wide_bytes` accessor and
    /// by the strider-py wrapper to surface the raw bytes to Python.
    #[must_use]
    pub fn to_le_bytes(&self) -> Vec<u8> {
        self.limbs().iter().flat_map(|l| l.to_le_bytes()).collect()
    }

}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Graph;

    #[test]
    fn intern_dedups_equal_u256_values() {
        let mut g = Graph::new();
        let v = WideConstStorage::I256([1, 2, 3, 4]);
        let id1 = g.intern_wide_const(v.clone());
        let id2 = g.intern_wide_const(v);
        assert_eq!(id1, id2, "interning the same value must return the same id");
    }

    #[test]
    fn intern_dedups_equal_u512_values() {
        let mut g = Graph::new();
        let v = WideConstStorage::I512([0xdead; 8]);
        let id1 = g.intern_wide_const(v.clone());
        let id2 = g.intern_wide_const(v);
        assert_eq!(id1, id2);
    }

    #[test]
    fn intern_assigns_distinct_ids_for_distinct_values() {
        let mut g = Graph::new();
        let id1 = g.intern_wide_const(WideConstStorage::I256([1; 4]));
        let id2 = g.intern_wide_const(WideConstStorage::I256([2; 4]));
        assert_ne!(id1, id2);
    }

    #[test]
    fn intern_distinguishes_u256_from_u512_with_same_low_limbs() {
        let mut g = Graph::new();
        let id_256 = g.intern_wide_const(WideConstStorage::I256([1, 0, 0, 0]));
        let id_512 = g.intern_wide_const(WideConstStorage::I512([1, 0, 0, 0, 0, 0, 0, 0]));
        assert_ne!(id_256, id_512);
    }

    #[test]
    fn wide_const_lookup_returns_stored_value() {
        let mut g = Graph::new();
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
