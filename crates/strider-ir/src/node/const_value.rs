use cranelift_entity::entity_impl;

/// Opaque; read the value through [`crate::IRViewer::int_const_u128`],
/// [`crate::IRViewer::int_const_i128`] or
/// [`crate::IRViewer::int_const_wide_le_bytes`].
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ConstId(u32);
entity_impl!(ConstId, "const");

#[derive(Clone, PartialEq, Eq, Hash)]
pub enum ConstValue {
    Bits(u64),
    /// Little-endian limbs, `limbs[0]` low.
    Wide(Box<[u64]>),
}

/// One spelling per value, never per arm: `Bits` within the `u128` carrier,
/// the limbs past it, whichever variant holds it. A derived `Debug` would
/// spell a 96-bit constant two ways, and the raw DOT label renders this, so
/// moving the carrier would move every graph holding one.
impl core::fmt::Debug for ConstValue {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Bits(v) => f.debug_tuple("Bits").field(v).finish(),
            Self::Wide(limbs) => match self.fits_u128() {
                Some(v) => f.debug_tuple("Bits").field(&v).finish(),
                None => f.debug_tuple("Wide").field(limbs).finish(),
            },
        }
    }
}

/// `Bits(u64)` keeps the alignment at 8 and leaves `Box`'s non-null niche to
/// carry the tag, so the enum is exactly its widest payload. A `u128` payload
/// costs 32: alignment 16, and no niche to hide the tag in.
const _: () = assert!(size_of::<ConstValue>() == 16);

impl ConstValue {
    /// `Wide` fits only when every limb above the low two is zero.
    pub fn fits_u128(&self) -> Option<u128> {
        match self {
            Self::Bits(v) => Some(u128::from(*v)),
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

    /// A user-supplied rewrite can reach `intern_int_const` with a float type
    /// (the pattern template materialiser guards only on WIDTH, and every
    /// float type is <= 128 bits).  Panicking there surfaces in Python as a
    /// `PanicException`, which derives from `BaseException` and so escapes
    /// `except Exception`; `validate` reports the same mistake catchably.
    #[test]
    fn intern_int_const_on_a_float_type_does_not_panic() {
        use crate::node::ValueType;
        let mut f = test_function();
        let id = f.intern_int_const(0, ValueType::float(64));
        assert_eq!(*f.const_value(id), ConstValue::Bits(0));
    }

    /// No unmasked value may slip in via the limb path.
    #[test]
    fn intern_int_const_limbs_masks_to_width() {
        use crate::node::ValueType;
        let mut f = test_function();
        // Bit 69 is inside I80's width, bit 84 is above it; only 69 survives.
        let hi: u64 = (1 << 5) | (1 << 20);
        let value = u128::from(hi) << 64;
        let limbed = f.intern_int_const_limbs(&[0, hi], ValueType::int(80));
        let scalar = f.intern_int_const(value, ValueType::int(80));
        assert_eq!(
            limbed, scalar,
            "limb path must mask to width like the scalar path"
        );
        let masked = ValueType::int(80).wrap_u128(value);
        assert_eq!(f.const_value(limbed).fits_u128(), Some(masked));
        assert_eq!(
            f.const_value(limbed),
            &ConstValue::Wide(vec![0, 1 << 5].into_boxed_slice()),
            "past the carrier the limbs carry the masked value"
        );
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
            let id1 = g.side_tables_mut().const_interner.intern(v.clone());
            let id2 = g.side_tables_mut().const_interner.intern(v);
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
            let id_a = g.side_tables_mut().const_interner.intern(a);
            let id_b = g.side_tables_mut().const_interner.intern(b);
            assert_ne!(id_a, id_b, "{label}: distinct values must get distinct ids");
        }
    }

    #[test]
    fn const_value_lookup_returns_stored_value() {
        let mut g = test_function();
        let v = ConstValue::Wide(vec![0x1234, 0x5678, 0x9abc, 0xdef0].into_boxed_slice());
        let id = g.side_tables_mut().const_interner.intern(v.clone());
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

    /// Both sides of the 64-bit carrier: one `ConstId` whichever path
    /// interned the value, read back masked to the width, serialised
    /// little-endian.
    #[test]
    fn carrier_boundary_values_intern_once_and_read_back() {
        use crate::node::ValueType;
        let cases: [(&str, u128, ValueType); 7] = [
            ("i63_all_ones_at_i64", (1u128 << 63) - 1, ValueType::int(64)),
            ("i64_all_ones", u128::from(u64::MAX), ValueType::int(64)),
            ("bit_64_at_i72", 1u128 << 64, ValueType::int(72)),
            ("i65_all_ones_at_i72", (1u128 << 65) - 1, ValueType::int(72)),
            (
                "i70_mixed_at_i80",
                (1u128 << 69) | 0xDEAD_BEEF,
                ValueType::int(80),
            ),
            (
                "i127_all_ones_at_i128",
                (1u128 << 127) - 1,
                ValueType::int(128),
            ),
            ("bit_127_at_i128", 1u128 << 127, ValueType::int(128)),
        ];
        for (label, value, ty) in cases {
            let mut f = test_function();
            let scalar = f.intern_int_const(value, ty);
            let limbed = f.intern_int_const_limbs(&[value as u64, (value >> 64) as u64], ty);
            assert_eq!(scalar, limbed, "{label}: both paths must share one id");
            assert_eq!(f.const_value(scalar).fits_u128(), Some(value), "{label}");
            let mut want = value.to_le_bytes().to_vec();
            want.resize(ty.byte_size(), 0);
            assert_eq!(
                f.const_value(scalar).to_le_bytes(ty.byte_size()),
                want,
                "{label}"
            );
        }
    }

    /// Past the carrier there is no scalar reading, and the limbs stand.
    #[test]
    fn values_past_the_u128_carrier_keep_their_limbs() {
        use crate::node::ValueType;
        let mut f = test_function();
        // Bit 128 is the first bit no `u128` carries.
        let id = f.intern_int_const_limbs(&[0, 0, 1, 0], ValueType::int(256));
        assert_eq!(f.const_value(id).fits_u128(), None);
        let mut want = vec![0u8; 32];
        want[16] = 1;
        assert_eq!(f.const_value(id).to_le_bytes(32), want);
    }

    /// Bits above the declared width are not part of the value, on either
    /// path, so an over-wide spelling dedups with the masked one.
    #[test]
    fn interning_masks_to_the_declared_width_on_both_paths() {
        use crate::node::ValueType;
        let cases: [(&str, ValueType); 4] = [
            ("i1", ValueType::I1),
            ("i64", ValueType::int(64)),
            ("i72", ValueType::int(72)),
            ("i128", ValueType::int(128)),
        ];
        for (label, ty) in cases {
            let mut f = test_function();
            let masked = ty.wrap_u128(u128::MAX);
            let saturated = f.intern_int_const(u128::MAX, ty);
            let exact = f.intern_int_const(masked, ty);
            let limbed = f.intern_int_const_limbs(&[u64::MAX; 8], ty);
            assert_eq!(saturated, exact, "{label}: the mask is applied first");
            assert_eq!(saturated, limbed, "{label}: the limb path masks too");
            assert_eq!(
                f.const_value(saturated).fits_u128(),
                Some(masked),
                "{label}"
            );
        }
    }

    /// `Debug` spells a value, not an arm: limb padding and which variant
    /// holds it must not show. The raw DOT label renders this, so a derived
    /// `Debug` would move every rendered graph holding a constant over the
    /// 64-bit carrier.
    #[test]
    fn debug_spells_the_value_not_the_arm() {
        let cases: [(&str, ConstValue, &str); 7] = [
            ("zero_bits", ConstValue::Bits(0), "Bits(0)"),
            (
                "zero_no_limbs",
                ConstValue::Wide(vec![].into_boxed_slice()),
                "Bits(0)",
            ),
            (
                "zero_padded",
                ConstValue::Wide(vec![0, 0, 0, 0].into_boxed_slice()),
                "Bits(0)",
            ),
            (
                "five_padded",
                ConstValue::Wide(vec![5, 0, 0, 0].into_boxed_slice()),
                "Bits(5)",
            ),
            (
                "bit_64",
                ConstValue::Wide(vec![0, 1].into_boxed_slice()),
                "Bits(18446744073709551616)",
            ),
            // The exact text the raw DOT label carries for a 96-bit mask.
            (
                "i96_all_ones",
                ConstValue::Wide(vec![u64::MAX, u64::from(u32::MAX)].into_boxed_slice()),
                "Bits(79228162514264337593543950335)",
            ),
            (
                "past_the_carrier",
                ConstValue::Wide(vec![0, 0, 1, 0].into_boxed_slice()),
                "Wide([0, 0, 1, 0])",
            ),
        ];
        for (label, value, want) in cases {
            assert_eq!(format!("{value:?}"), want, "{label}");
        }
    }

    /// Every route to one value renders alike, declared width included.
    #[test]
    fn interned_values_render_alike_whatever_route_reached_them() {
        use crate::node::ValueType;
        let mut f = test_function();
        let value: u128 = (1u128 << 69) | 0xDEAD_BEEF;
        let ids = [
            f.intern_int_const(value, ValueType::int(128)),
            f.intern_int_const_limbs(&[0xDEAD_BEEF, 1 << 5], ValueType::int(128)),
            f.intern_int_const_limbs(&[0xDEAD_BEEF, 1 << 5, 0, 0], ValueType::int(256)),
            f.intern_int_const_limbs(&[0xDEAD_BEEF, 1 << 5], ValueType::int(80)),
        ];
        let debugs: Vec<String> = ids
            .iter()
            .map(|&id| format!("{:?}", f.const_value(id)))
            .collect();
        assert_eq!(debugs[0], "Bits(590295810362441580271)");
        assert!(
            debugs.iter().all(|d| *d == debugs[0]),
            "one value, one Debug spelling: {debugs:?}"
        );
    }

    #[test]
    fn to_le_bytes_serialises_little_endian() {
        // Bytes 1..=8 little-endian.
        let low: u64 = 0x0807_0605_0403_0201;
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
        let wide = ConstValue::Wide(vec![low, 0, 0, 0].into_boxed_slice());
        let mut expected: Vec<u8> = (1u8..=8).collect();
        expected.extend(std::iter::repeat_n(0u8, 24));
        assert_eq!(wide.to_le_bytes(32), expected);
    }
}
