//! Interned integer-constant values. Every `NodeKind::IntConst(ConstId)`
//! references one entry in `crate::Function::const_interner`.
//!
//! Storage dedups by VALUE MAGNITUDE: a value that fits `u128` is `Bits`
//! (covers I1..I512 whose value ≤ u128); a value that needs more than 128
//! bits is `Wide` (boxed little-endian limbs, I256/I512). The constant's
//! WIDTH is carried by the node's output `ValueKind`, never by this storage,
//! so `IntConst(42):I80` and `IntConst(42):I128` share one `ConstId` and are
//! distinguished only at the node level (different output kind ⇒ different
//! dedup-cache key). Read values through `crate::IRViewer` accessors.

use cranelift_entity::entity_impl;

/// Dense id of an interned integer-constant value
/// (`crate::Function::const_interner`). Opaque; resolve via
/// `crate::Function::const_value`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ConstId(u32);
entity_impl!(ConstId, "const");

/// The interned value of an integer constant.
///
/// `Bits` holds any value ≤ 128 bits inline. `Wide` boxes the little-endian
/// limbs of a value that exceeds 128 bits (`limbs[0]` low, `limbs[N-1]` high);
/// only I256 (4 limbs) / I512 (8 limbs) reach it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ConstValue {
    /// Value ≤ 128 bits, held inline.
    Bits(u128),
    /// Value > 128 bits, boxed little-endian limbs.
    Wide(Box<[u64]>),
}

impl ConstValue {
    /// The value as `u128` if it fits (always for `Bits`; for `Wide` only
    /// when every limb above the low two is zero), else `None`.
    pub fn fits_u128(&self) -> Option<u128> {
        match self {
            Self::Bits(v) => Some(*v),
            Self::Wide(limbs) => {
                if limbs.iter().skip(2).all(|&l| l == 0) {
                    let lo = u128::from(*limbs.first().unwrap_or(&0));
                    let hi = u128::from(limbs.get(1).copied().unwrap_or(0));
                    Some((hi << 64) | lo)
                } else {
                    None
                }
            }
        }
    }

    /// Little-endian bytes zero-extended / truncated to `byte_size`.
    pub fn to_le_bytes(&self, byte_size: usize) -> Vec<u8> {
        let mut out = vec![0u8; byte_size];
        match self {
            Self::Bits(v) => {
                let b = v.to_le_bytes();
                let n = byte_size.min(b.len());
                out[..n].copy_from_slice(&b[..n]);
            }
            Self::Wide(limbs) => {
                let bytes: Vec<u8> = limbs.iter().flat_map(|limb| limb.to_le_bytes()).collect();
                let n = byte_size.min(bytes.len());
                out[..n].copy_from_slice(&bytes[..n]);
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::function::test_function;

    /// The limb path masks a fits-`u128` value to the declared width, exactly
    /// like the scalar path — so no unmasked `Bits` can slip in via limbs.
    #[test]
    fn intern_int_const_limbs_masks_fits_u128_to_width() {
        use crate::node::ValueType;
        let mut f = test_function();
        // High limb mixes one bit inside I80's 80-bit width (bit 69) and one
        // above it (bit 84); only the in-width bit must survive.
        let hi: u64 = (1 << 5) | (1 << 20);
        let value = u128::from(hi) << 64;
        let limbed = f.intern_int_const_limbs(&[0, hi], ValueType::I80);
        let scalar = f.intern_int_const(value, ValueType::I80);
        assert_eq!(
            limbed, scalar,
            "limb path must mask to width like the scalar path"
        );
        match f.const_value(limbed) {
            ConstValue::Bits(v) => {
                assert_eq!(*v, value & ValueType::I80.bit_mask_u128());
                assert_eq!(
                    *v & !ValueType::I80.bit_mask_u128(),
                    0,
                    "no bits above width"
                );
            }
            other => panic!("expected Bits, got {other:?}"),
        }
    }

    /// Interning the same value twice returns the same id, for both the inline
    /// `Bits` form and the boxed `Wide` form.
    #[test]
    fn intern_dedups_equal_values() {
        // Rows: (label, value).
        let cases: [(&str, ConstValue); 2] = [
            ("intern_dedups_equal_bits", ConstValue::Bits(0xABCD)),
            (
                "intern_dedups_equal_wide",
                ConstValue::Wide(vec![1, 2, 3, 4].into_boxed_slice()),
            ),
        ];
        for (label, v) in cases {
            let mut g = test_function();
            let id1 = g.const_interner.intern(v.clone());
            let id2 = g.const_interner.intern(v);
            assert_eq!(
                id1, id2,
                "{label}: interning the same value must return the same id"
            );
        }
    }

    /// Distinct values get distinct ids.
    #[test]
    fn intern_assigns_distinct_ids_for_distinct_values() {
        let cases: [(&str, ConstValue, ConstValue); 2] = [
            ("distinct_bits", ConstValue::Bits(1), ConstValue::Bits(2)),
            (
                "distinct_wide",
                ConstValue::Wide(vec![1, 0, 0, 0].into_boxed_slice()),
                ConstValue::Wide(vec![2, 0, 0, 0].into_boxed_slice()),
            ),
        ];
        for (label, a, b) in cases {
            let mut g = test_function();
            let id_a = g.const_interner.intern(a);
            let id_b = g.const_interner.intern(b);
            assert_ne!(id_a, id_b, "{label}: distinct values must get distinct ids");
        }
    }

    #[test]
    fn const_value_lookup_returns_stored_value() {
        let mut g = test_function();
        let v = ConstValue::Wide(vec![0x1234, 0x5678, 0x9abc, 0xdef0].into_boxed_slice());
        let id = g.const_interner.intern(v.clone());
        assert_eq!(g.const_value(id), &v);
    }

    /// `fits_u128`: `Bits(v)` always fits; a `Wide` whose high limbs are zero
    /// fits; a `Wide` with a nonzero limb above the low two does not.
    #[test]
    fn fits_u128_cases() {
        assert_eq!(ConstValue::Bits(0xDEAD_BEEF).fits_u128(), Some(0xDEAD_BEEF));
        assert_eq!(
            ConstValue::Wide(vec![1, 0, 0, 0].into_boxed_slice()).fits_u128(),
            Some(1)
        );
        // limb index 2 nonzero ⇒ exceeds 128 bits.
        assert_eq!(
            ConstValue::Wide(vec![0, 0, 1, 0].into_boxed_slice()).fits_u128(),
            None
        );
    }

    /// `to_le_bytes` serialises little-endian, zero-padded / truncated to the
    /// requested byte size, for both `Bits` and `Wide`.
    #[test]
    fn to_le_bytes_serialises_little_endian() {
        // The low 8 bytes are 0x0807_0605_0403_0201 — bytes 1..=8 LE.
        let low: u128 = 0x0807_0605_0403_0201;
        // Bits at byte_size 10 (I80 width): low 8 bytes then zero pad.
        assert_eq!(
            ConstValue::Bits(low).to_le_bytes(10),
            vec![1, 2, 3, 4, 5, 6, 7, 8, 0, 0]
        );
        // Bits at byte_size 16 (I128 width).
        assert_eq!(
            ConstValue::Bits(low).to_le_bytes(16),
            vec![1, 2, 3, 4, 5, 6, 7, 8, 0, 0, 0, 0, 0, 0, 0, 0]
        );
        // Wide([low, 0, 0, 0]) at byte_size 32 (I256 width).
        let wide = ConstValue::Wide(vec![low as u64, 0, 0, 0].into_boxed_slice());
        let mut expected: Vec<u8> = (1u8..=8).collect();
        expected.extend(std::iter::repeat_n(0u8, 24));
        assert_eq!(wide.to_le_bytes(32), expected);
    }
}
