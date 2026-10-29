use cranelift_entity::entity_impl;

use crate::node::ValueType;

/// Opaque; read the value through [`crate::IRViewer::int_const_u128`],
/// [`crate::IRViewer::int_const_i128`] or
/// [`crate::IRViewer::int_const_wide_le_bytes`].
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ConstId(u32);
entity_impl!(ConstId, "const");

/// An integer constant of any supported width. Built through
/// [`ConstValue::bits`] / [`ConstValue::wide`], read through
/// [`ConstValue::get_value`].
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct ConstValue(Repr);

/// Private, so the only way in is through the constructors and the only way
/// out is [`ConstValue::get_value`]: one value has one `Repr`, which is what
/// the interner dedups by.
#[derive(Clone, PartialEq, Eq, Hash)]
enum Repr {
    Bits(u64),
    /// Little-endian limbs, `limbs[0]` low.
    Wide(Box<[u64]>),
}

/// Limbs a `ty`-wide value spans.
fn limb_count(ty: ValueType) -> usize {
    ty.bit_width().div_ceil(64)
}

/// A value read through [`ConstValue::get_value`]: one reading per value,
/// never per arm.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConstRead<'a> {
    /// Within the `u128` carrier.
    Val(u128),
    /// Past it, little-endian limbs, `limbs[0]` low.
    Wide(&'a [u64]),
}

/// One spelling per value, never per arm. A derived `Debug` would spell a
/// 96-bit constant two ways, and the raw DOT label renders this, so moving
/// the carrier would move every graph holding one.
impl core::fmt::Debug for ConstValue {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self.get_value() {
            ConstRead::Val(v) => f.debug_tuple("Bits").field(&v).finish(),
            ConstRead::Wide(limbs) => f.debug_tuple("Wide").field(&limbs).finish(),
        }
    }
}

/// `Bits(u64)` keeps the alignment at 8 and leaves `Box`'s non-null niche to
/// carry the tag, so `Repr` is exactly its widest payload. A `u128` payload
/// costs 32: alignment 16, and no niche to hide the tag in.
const _: () = assert!(size_of::<ConstValue>() == 16);

impl ConstValue {
    /// A value the 64-bit carrier holds.
    pub fn bits(v: u64) -> Self {
        Self(Repr::Bits(v))
    }

    /// Little-endian `limbs`, `limbs[0]` low, masked to the declared width by
    /// the caller. A value the carrier holds collapses to [`Self::bits`], so
    /// the scalar and the limb path reach one value and the interner gives
    /// them one id.
    pub fn wide(limbs: Box<[u64]>) -> Self {
        if limbs.iter().skip(1).all(|&l| l == 0) {
            Self::bits(limbs.first().copied().unwrap_or(0))
        } else {
            Self(Repr::Wide(limbs))
        }
    }

    /// Every bit of `ty` set.
    pub fn all_ones(ty: ValueType) -> Self {
        Self::wide(vec![u64::MAX; limb_count(ty)].into_boxed_slice()).mask(ty)
    }

    /// `!self` within `ty`. Limb-wise, so it is exact at `I256` / `I512` too,
    /// where no `u128` holds the value.
    pub fn complement(&self, ty: ValueType) -> Self {
        let limbs: Vec<u64> = self.limbs_to(limb_count(ty)).iter().map(|l| !l).collect();
        Self::wide(limbs.into_boxed_slice()).mask(ty)
    }

    /// `self` with every bit above `ty`'s width cleared. Limb-wise, as
    /// [`Self::complement`] is.
    pub fn mask(&self, ty: ValueType) -> Self {
        let limb_count = limb_count(ty);
        let mut limbs = self.limbs_to(limb_count);
        if let Some(last) = limbs.last_mut() {
            // `limb_count` is `ceil(width / 64)`, so the top limb carries
            // 1..=64 live bits and `spare` is below 64.
            let spare = limb_count * 64 - ty.bit_width();
            *last &= u64::MAX >> spare;
        }
        Self::wide(limbs.into_boxed_slice())
    }

    /// Little-endian limbs, zero-padded or truncated to `n`.
    fn limbs_to(&self, n: usize) -> Vec<u64> {
        let mut out = vec![0u64; n];
        let src: &[u64] = match &self.0 {
            Repr::Bits(v) => core::slice::from_ref(v),
            Repr::Wide(limbs) => limbs,
        };
        for (slot, limb) in out.iter_mut().zip(src) {
            *slot = *limb;
        }
        out
    }

    /// The one read. `Wide` answers `Val` too whenever it fits, which is
    /// every limb above the low two being zero.
    pub fn get_value(&self) -> ConstRead<'_> {
        match &self.0 {
            Repr::Bits(v) => ConstRead::Val(u128::from(*v)),
            Repr::Wide(limbs) => {
                if limbs.iter().skip(2).all(|&l| l == 0) {
                    let lo = u128::from(limbs.first().copied().unwrap_or(0));
                    let hi = u128::from(limbs.get(1).copied().unwrap_or(0));
                    ConstRead::Val((hi << 64) | lo)
                } else {
                    ConstRead::Wide(limbs)
                }
            }
        }
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
        assert_eq!(*f.const_value(id), ConstValue::bits(0));
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
        assert_eq!(f.const_value(limbed).get_value(), ConstRead::Val(masked));
        assert_eq!(
            f.const_value(limbed),
            &ConstValue::wide(vec![0, 1 << 5].into_boxed_slice()),
            "past the carrier the limbs carry the masked value"
        );
    }

    /// Total at every supported integer width, the two past the `u128`
    /// carrier included: all-ones has exactly `bit_width` bits set, and
    /// complementing it twice is the identity.
    #[test]
    fn all_ones_and_complement_are_total_across_every_width() {
        use crate::node::ValueType;
        for ty in ValueType::SUPPORTED.into_iter().filter(|t| t.is_integer()) {
            let ones = ConstValue::all_ones(ty);
            assert_eq!(set_bits(&ones), ty.bit_width(), "{ty:?}");
            assert_eq!(
                ones.complement(ty),
                ConstValue::bits(0),
                "{ty:?}: all-ones complements to zero"
            );
            assert_eq!(
                ConstValue::bits(0).complement(ty),
                ones,
                "{ty:?}: zero complements to all-ones"
            );
            assert_eq!(ones.mask(ty), ones, "{ty:?}: masking all-ones is a no-op");
        }
    }

    /// The widths a `u128` cannot hold: every bit is addressable, and nothing
    /// above the declared width survives.
    #[test]
    fn complement_and_mask_are_exact_past_the_carrier() {
        use crate::node::ValueType;
        let i256 = ValueType::int(256);
        // Bit 200 alone, well above the carrier.
        let v = ConstValue::wide(vec![0, 0, 0, 1 << 8].into_boxed_slice());
        assert_eq!(set_bits(&v), 1);
        let c = v.complement(i256);
        assert_eq!(set_bits(&c), 255);
        assert_eq!(c.complement(i256), v, "complement is an involution at I256");
        assert_eq!(
            ConstValue::all_ones(i256).get_value(),
            ConstRead::Wide(&[u64::MAX; 4])
        );
        assert_eq!(
            ConstValue::all_ones(ValueType::int(512)).get_value(),
            ConstRead::Wide(&[u64::MAX; 8])
        );
        // A value spelled wider than its type keeps only the declared bits.
        assert_eq!(
            ConstValue::wide(vec![u64::MAX; 8].into_boxed_slice()).mask(i256),
            ConstValue::all_ones(i256)
        );
        // I256 and I512 fill whole limbs, so what pins the trailing mask is a
        // multi-limb width whose top limb is only partly live: I112, 48 spare.
        let i112 = ValueType::int(112);
        assert_eq!(
            ConstValue::bits(0).complement(i112).get_value(),
            ConstRead::Val((1u128 << 112) - 1)
        );
        assert_eq!(
            ConstValue::wide(vec![0, 1 << 48].into_boxed_slice()).complement(i112),
            ConstValue::wide(vec![u64::MAX, (1u64 << 48) - 1].into_boxed_slice()),
            "bit 112 is above the width, so complementing it leaves nothing there"
        );
        // I72: the top limb is live for 8 bits only.
        let i72 = ValueType::int(72);
        assert_eq!(
            ConstValue::all_ones(i72).get_value(),
            ConstRead::Val((1u128 << 72) - 1)
        );
        assert_eq!(
            ConstValue::wide(vec![u64::MAX, u64::MAX].into_boxed_slice()).mask(i72),
            ConstValue::all_ones(i72)
        );
        assert_eq!(
            ConstValue::bits(0xFF).complement(i72).get_value(),
            ConstRead::Val(((1u128 << 72) - 1) ^ 0xFF)
        );
    }

    fn set_bits(v: &ConstValue) -> usize {
        match v.get_value() {
            ConstRead::Val(v) => v.count_ones() as usize,
            ConstRead::Wide(limbs) => limbs.iter().map(|l| l.count_ones() as usize).sum(),
        }
    }

    #[test]
    fn intern_dedups_equal_values() {
        let cases: [(&str, ConstValue); 2] = [
            ("intern_dedups_equal_bits", ConstValue::bits(0xABCD)),
            (
                "intern_dedups_equal_wide",
                ConstValue::wide(vec![1, 2, 3, 4].into_boxed_slice()),
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
            ("distinct_bits", ConstValue::bits(1), ConstValue::bits(2)),
            (
                "distinct_wide",
                ConstValue::wide(vec![1, 0, 0, 0].into_boxed_slice()),
                ConstValue::wide(vec![2, 0, 0, 0].into_boxed_slice()),
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
        let v = ConstValue::wide(vec![0x1234, 0x5678, 0x9abc, 0xdef0].into_boxed_slice());
        let id = g.side_tables_mut().const_interner.intern(v.clone());
        assert_eq!(g.const_value(id), &v);
    }

    /// `Val` up to the carrier whichever variant holds the value, `Wide` only
    /// past it.
    #[test]
    fn get_value_cases() {
        let wide = |l: &[u64]| ConstValue::wide(l.to_vec().into_boxed_slice());
        let bits = ConstValue::bits(0xDEAD_BEEF);
        assert_eq!(bits.get_value(), ConstRead::Val(0xDEAD_BEEF));
        assert_eq!(wide(&[1, 0, 0, 0]).get_value(), ConstRead::Val(1));
        assert_eq!(wide(&[0, 0, 0, 0]).get_value(), ConstRead::Val(0));
        assert_eq!(wide(&[]).get_value(), ConstRead::Val(0));
        // Both limbs nonzero, the carrier exactly full.
        assert_eq!(
            wide(&[u64::MAX, u64::MAX]).get_value(),
            ConstRead::Val(u128::MAX)
        );
        // A nonzero limb at index 2 puts the value over 128 bits.
        assert_eq!(
            wide(&[0, 0, 1, 0]).get_value(),
            ConstRead::Wide(&[0, 0, 1, 0])
        );
    }

    /// Both sides of the 64-bit carrier: one `ConstId` whichever path
    /// interned the value, read back masked to the width.
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
            assert_eq!(
                f.const_value(scalar).get_value(),
                ConstRead::Val(value),
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
        assert_eq!(
            f.const_value(id).get_value(),
            ConstRead::Wide(&[0, 0, 1, 0])
        );
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
                f.const_value(saturated).get_value(),
                ConstRead::Val(masked),
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
            ("zero_bits", ConstValue::bits(0), "Bits(0)"),
            (
                "zero_no_limbs",
                ConstValue::wide(vec![].into_boxed_slice()),
                "Bits(0)",
            ),
            (
                "zero_padded",
                ConstValue::wide(vec![0, 0, 0, 0].into_boxed_slice()),
                "Bits(0)",
            ),
            (
                "five_padded",
                ConstValue::wide(vec![5, 0, 0, 0].into_boxed_slice()),
                "Bits(5)",
            ),
            (
                "bit_64",
                ConstValue::wide(vec![0, 1].into_boxed_slice()),
                "Bits(18446744073709551616)",
            ),
            // The exact text the raw DOT label carries for a 96-bit mask.
            (
                "i96_all_ones",
                ConstValue::wide(vec![u64::MAX, u64::from(u32::MAX)].into_boxed_slice()),
                "Bits(79228162514264337593543950335)",
            ),
            (
                "past_the_carrier",
                ConstValue::wide(vec![0, 0, 1, 0].into_boxed_slice()),
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
}
