use cranelift_entity::entity_impl;

/// Opaque; resolve via `crate::Function::const_value`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ConstId(u32);
entity_impl!(ConstId, "const");

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ConstValue {
    Bits(u128),
    /// Little-endian limbs, `limbs[0]` low.
    Wide(Box<[u64]>),
}

impl ConstValue {
    /// `Wide` fits only when every limb above the low two is zero.
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

    /// No unmasked `Bits` may slip in via the limb path.
    #[test]
    fn intern_int_const_limbs_masks_fits_u128_to_width() {
        use crate::node::ValueType;
        let mut f = test_function();
        // Bit 69 is inside I80's width, bit 84 is above it; only 69 survives.
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

    #[test]
    fn intern_dedups_equal_values() {
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

    #[test]
    fn fits_u128_cases() {
        assert_eq!(ConstValue::Bits(0xDEAD_BEEF).fits_u128(), Some(0xDEAD_BEEF));
        assert_eq!(
            ConstValue::Wide(vec![1, 0, 0, 0].into_boxed_slice()).fits_u128(),
            Some(1)
        );
        // A nonzero limb at index 2 puts the value over 128 bits.
        assert_eq!(
            ConstValue::Wide(vec![0, 0, 1, 0].into_boxed_slice()).fits_u128(),
            None
        );
    }

    #[test]
    fn to_le_bytes_serialises_little_endian() {
        // Bytes 1..=8 little-endian.
        let low: u128 = 0x0807_0605_0403_0201;
        // I80 width: low 8 bytes then zero pad.
        assert_eq!(
            ConstValue::Bits(low).to_le_bytes(10),
            vec![1, 2, 3, 4, 5, 6, 7, 8, 0, 0]
        );
        // I128 width.
        assert_eq!(
            ConstValue::Bits(low).to_le_bytes(16),
            vec![1, 2, 3, 4, 5, 6, 7, 8, 0, 0, 0, 0, 0, 0, 0, 0]
        );
        // I256 width.
        let wide = ConstValue::Wide(vec![low as u64, 0, 0, 0].into_boxed_slice());
        let mut expected: Vec<u8> = (1u8..=8).collect();
        expected.extend(std::iter::repeat_n(0u8, 24));
        assert_eq!(wide.to_le_bytes(32), expected);
    }
}
