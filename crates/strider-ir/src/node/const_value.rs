use cranelift_entity::entity_impl;

/// Opaque; read the value through [`crate::IRViewer::int_const`] or its
/// width-aware forms [`crate::IRViewer::int_const_u128`] and
/// [`crate::IRViewer::int_const_i128`].
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ConstId(u32);
entity_impl!(ConstId, "const");

#[cfg(test)]
mod tests {
    use dashu_int::UBig;

    use crate::function::test_function;
    use crate::node::ValueType;

    /// A user-supplied rewrite can reach `intern_int_const` with a float type
    /// (the pattern template materialiser guards only on WIDTH, and every
    /// float type is <= 128 bits).  Panicking there surfaces in Python as a
    /// `PanicException`, which derives from `BaseException` and so escapes
    /// `except Exception`; `validate` reports the same mistake catchably.
    #[test]
    fn intern_int_const_on_a_float_type_does_not_panic() {
        let mut f = test_function();
        let id = f.intern_int_const(0, ValueType::float(64));
        assert_eq!(*f.const_value(id), UBig::ZERO);
    }

    /// No unmasked value may slip in via the limb path.
    #[test]
    fn intern_int_const_limbs_masks_to_width() {
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
        assert_eq!(u128::try_from(f.const_value(limbed)), Ok(masked));
        assert_eq!(
            f.const_value(limbed),
            &UBig::from_words(&[0, 1 << 5]),
            "past the carrier the limbs carry the masked value"
        );
    }

    #[test]
    fn intern_dedups_equal_values() {
        let cases: [(&str, UBig); 2] = [
            ("intern_dedups_equal_bits", UBig::from(0xABCDu64)),
            ("intern_dedups_equal_wide", UBig::from_words(&[1, 2, 3, 4])),
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
        let cases: [(&str, UBig, UBig); 2] = [
            ("distinct_bits", UBig::from(1u64), UBig::from(2u64)),
            (
                "distinct_wide",
                UBig::from_words(&[1, 0, 0, 0]),
                UBig::from_words(&[2, 0, 0, 0]),
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
        let v = UBig::from_words(&[0x1234, 0x5678, 0x9abc, 0xdef0]);
        let id = g.side_tables_mut().const_interner.intern(v.clone());
        assert_eq!(g.const_value(id), &v);
    }

    /// Dedup rests on one value having one stored form whatever spelling
    /// reached it: zero-padded limbs, a short slice and the scalar path all
    /// hash and compare alike.
    #[test]
    fn construction_paths_normalise_to_one_stored_value() {
        let cases: [(&str, UBig, UBig); 4] = [
            ("zero", UBig::ZERO, UBig::from_words(&[0, 0, 0, 0])),
            ("small", UBig::from(5u64), UBig::from_words(&[5, 0, 0, 0])),
            (
                "short_vs_padded",
                UBig::from_words(&[7, 0, 1]),
                UBig::from_words(&[7, 0, 1, 0, 0, 0, 0, 0]),
            ),
            (
                "carrier_full",
                UBig::from(u128::MAX),
                UBig::from_words(&[u64::MAX, u64::MAX, 0, 0]),
            ),
        ];
        for (label, a, b) in cases {
            assert_eq!(a, b, "{label}: one value, one stored form");
            assert_eq!(a.as_words(), b.as_words(), "{label}: one limb spelling");
            let mut g = test_function();
            let id_a = g.side_tables_mut().const_interner.intern(a);
            let id_b = g.side_tables_mut().const_interner.intern(b);
            assert_eq!(id_a, id_b, "{label}: both paths intern to one id");
        }
    }

    /// `u128` up to the carrier whichever spelling holds the value, `Err`
    /// only past it.
    #[test]
    fn u128_read_cases() {
        let wide = UBig::from_words;
        assert_eq!(u128::try_from(&UBig::from(0xDEAD_BEEFu64)), Ok(0xDEAD_BEEF));
        assert_eq!(u128::try_from(&wide(&[1, 0, 0, 0])), Ok(1));
        assert_eq!(u128::try_from(&wide(&[0, 0, 0, 0])), Ok(0));
        assert_eq!(u128::try_from(&wide(&[])), Ok(0));
        // Both limbs nonzero, the carrier exactly full.
        assert_eq!(u128::try_from(&wide(&[u64::MAX, u64::MAX])), Ok(u128::MAX));
        // A nonzero limb at index 2 puts the value over 128 bits.
        assert!(u128::try_from(&wide(&[0, 0, 1, 0])).is_err());
        assert_eq!(
            crate::eval::words(&wide(&[0, 0, 1, 0]), ValueType::int(256)),
            vec![0, 0, 1, 0]
        );
    }

    /// Both sides of the 64-bit carrier: one `ConstId` whichever path
    /// interned the value, read back masked to the width.
    #[test]
    fn carrier_boundary_values_intern_once_and_read_back() {
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
            assert_eq!(u128::try_from(f.const_value(scalar)), Ok(value), "{label}");
        }
    }

    /// Past the carrier there is no scalar reading, and the limbs stand.
    #[test]
    fn values_past_the_u128_carrier_keep_their_limbs() {
        let mut f = test_function();
        // Bit 128 is the first bit no `u128` carries.
        let id = f.intern_int_const_limbs(&[0, 0, 1, 0], ValueType::int(256));
        assert!(u128::try_from(f.const_value(id)).is_err());
        assert_eq!(
            crate::eval::words(f.const_value(id), ValueType::int(256)),
            vec![0, 0, 1, 0]
        );
    }

    /// Bits above the declared width are not part of the value, on either
    /// path, so an over-wide spelling dedups with the masked one.
    #[test]
    fn interning_masks_to_the_declared_width_on_both_paths() {
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
                u128::try_from(f.const_value(saturated)),
                Ok(masked),
                "{label}"
            );
        }
    }
}
