//! What `encode` writes into an instruction field, `implicit_addend` reads
//! back; `encode` leaves every bit outside the field alone; and it refuses
//! exactly the values the field cannot hold.

use proptest::prelude::*;

use super::encodings::{Field, Half, Isa, encode, implicit_addend};

/// `v`, read as signed, lies in `lo..hi`.
fn within(v: u64, lo: i64, hi: i64) -> bool {
    (lo..hi).contains(&(v as i64))
}

/// A value of every magnitude: random bits sign-extended from a random width,
/// or one within two of a power of two, so each field's range ends are hit
/// from both sides.
fn value() -> impl Strategy<Value = u64> {
    prop_oneof![
        (1u32..=64, any::<u64>()).prop_map(|(bits, x)| {
            let shift = 64 - bits;
            (((x << shift) as i64) >> shift) as u64
        }),
        (0u32..64, -2i64..=1, any::<bool>()).prop_map(|(b, d, neg)| {
            let v = (1i64 << b).wrapping_add(d);
            (if neg { v.wrapping_neg() } else { v }) as u64
        }),
    ]
}

/// The fields of the RELA-only arches, which keep no addend in the field.
fn rela_only_field() -> impl Strategy<Value = Field> {
    use Half::{Ha, Hi, Higher, HigherA, Highest, HighestA, Lo, Signed, Whole};
    prop_oneof![
        Just(Field::A64Branch26),
        Just(Field::A64Imm19),
        Just(Field::A64Imm14),
        (any::<bool>(), any::<bool>()).prop_map(|(page, check)| Field::A64Adr { page, check }),
        (0u8..=4).prop_map(|shift| Field::A64Lo12 { shift }),
        // No relocation checks `G3`, whose range test would shift by 64.
        (0u8..=3, any::<bool>()).prop_map(|(group, check)| Field::A64Movw {
            group,
            check: check && group < 3,
        }),
        Just(Field::PpcBranch24),
        Just(Field::PpcBranch14),
        prop::sample::select(vec![
            Whole, Signed, Lo, Hi, Ha, Higher, HigherA, Highest, HighestA
        ])
        .prop_map(Field::PpcHalf16),
        prop::sample::select(vec![Signed, Lo]).prop_map(Field::PpcHalf16Ds),
    ]
}

/// The documented range of each RELA-only field.
fn rela_only_fits(field: Field, v: u64) -> bool {
    let aligned = v & 3 == 0;
    match field {
        Field::A64Branch26 => within(v, -(1 << 27), 1 << 27),
        Field::A64Imm19
        | Field::A64Adr {
            page: false,
            check: true,
        } => within(v, -(1 << 20), 1 << 20),
        Field::A64Imm14 | Field::PpcHalf16(Half::Signed) => within(v, -(1 << 15), 1 << 15),
        Field::A64Adr {
            page: true,
            check: true,
        } => within(v, -(1 << 32), 1 << 32),
        Field::A64Movw { group, check: true } => v >> (16 * u32::from(group) + 16) == 0,
        Field::PpcBranch24 => within(v, -(1 << 25), 1 << 25) && aligned,
        Field::PpcBranch14 | Field::PpcHalf16Ds(Half::Signed) => {
            within(v, -(1 << 15), 1 << 15) && aligned
        }
        Field::PpcHalf16(Half::Whole) => within(v, -(1 << 15), 1 << 16),
        Field::PpcHalf16Ds(_) => aligned,
        _ => true,
    }
}

/// The bits of a RELA-only field's `raw` that `encode` may change.
fn rela_only_mask(field: Field) -> u64 {
    match field {
        Field::A64Branch26 => 0x03ff_ffff,
        Field::A64Imm19 => 0x00ff_ffe0,
        Field::A64Imm14 => 0x0007_ffe0,
        Field::A64Adr { .. } => 0x60ff_ffe0,
        Field::A64Lo12 { .. } => 0x003f_fc00,
        Field::A64Movw { .. } => 0x001f_ffe0,
        Field::PpcBranch24 => 0x03ff_fffc,
        // A DS field leaves the instruction's extended opcode in bits 1:0.
        Field::PpcBranch14 | Field::PpcHalf16Ds(_) => 0xfffc,
        Field::PpcHalf16(_) => 0xffff,
        other => unreachable!("{other:?} keeps an implicit addend"),
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(4096))]

    #[test]
    fn data_round_trips(
        bytes in prop::sample::select(vec![1u8, 2, 4, 8]),
        signed in any::<bool>(),
        v in value(),
    ) {
        let field = Field::Data { bytes, signed };
        let bits = 8 * u32::from(bytes);
        let fits = bytes == 8
            || within(v, -(1 << (bits - 1)), if signed { 1 << (bits - 1) } else { 1 << bits });
        let encoded = encode(field, 0, v, Isa::Same, 0);
        prop_assert_eq!(encoded.is_some(), fits);
        if let Some(e) = encoded {
            let back = implicit_addend(field, e).expect("a data field keeps its addend");
            let low = u64::MAX >> (64 - bits);
            prop_assert_eq!(back as u64 & low, v & low);
            if signed {
                prop_assert_eq!(back, v as i64);
            }
        }
    }

    #[test]
    fn arm_branch_round_trips(call in any::<bool>(), raw in any::<u32>(), v in value()) {
        let raw = u64::from(raw);
        // A non-call type over a `BLX` keeps its H bit, which reads back as
        // offset bit 1; no assembler emits that pair.
        prop_assume!(call || raw >> 28 != 0xf);
        let field = Field::ArmBranch24 { call };
        let encoded = encode(field, raw, v, Isa::Same, 0);
        prop_assert_eq!(encoded.is_some(), within(v, -(1 << 25), 1 << 25));
        if let Some(e) = encoded {
            prop_assert_eq!(implicit_addend(field, e), Some((v & !3) as i64));
            // A `BLX` reaching a target in the caller's own state becomes a `BL`.
            let top = if call && raw >> 28 == 0xf { 0xeb00_0000 } else { raw & 0xff00_0000 };
            prop_assert_eq!(e & !0x00ff_ffff, top);
        }
    }

    #[test]
    fn arm_prel31_round_trips(raw in any::<u32>(), v in value()) {
        let raw = u64::from(raw);
        let encoded = encode(Field::ArmPrel31, raw, v, Isa::Same, 0);
        prop_assert_eq!(encoded.is_some(), within(v, -(1 << 30), 1 << 30));
        if let Some(e) = encoded {
            prop_assert_eq!(implicit_addend(Field::ArmPrel31, e), Some(v as i64));
            prop_assert_eq!(e & !0x7fff_ffff, raw & 0x8000_0000);
        }
    }

    #[test]
    fn movw_round_trips(
        top in any::<bool>(),
        thumb in any::<bool>(),
        raw in any::<u32>(),
        v in value(),
    ) {
        let raw = u64::from(raw);
        let (field, field_bits) = if thumb {
            (Field::ThumbMovw { top }, 0x040f_70ff)
        } else {
            (Field::ArmMovw { top }, 0x000f_0fff)
        };
        let e = encode(field, raw, v, Isa::Same, 0)
            .expect("a MOVW / MOVT immediate never overflows");
        let imm = (if top { v >> 16 } else { v }) & 0xffff;
        prop_assert_eq!(implicit_addend(field, e).map(|a| a as u64 & 0xffff), Some(imm));
        prop_assert_eq!(e & !field_bits, raw & !field_bits);
    }

    #[test]
    fn thumb_branch_round_trips(call in any::<bool>(), raw in any::<u32>(), v in value()) {
        let raw = u64::from(raw);
        let field = Field::ThumbBranch { call };
        let encoded = encode(field, raw, v, Isa::Same, 0);
        prop_assert_eq!(encoded.is_some(), within(v, -(1 << 24), 1 << 24));
        if let Some(e) = encoded {
            prop_assert_eq!(implicit_addend(field, e), Some((v & !1) as i64));
            // Bit 12 of the second halfword is set for `BL`, clear for `BLX`.
            let kept = if call { 0xf800_c000 } else { 0xf800_d000 };
            prop_assert_eq!(e & kept, raw & kept);
            if call {
                prop_assert_eq!(e & 0x1000, 0x1000);
            }
        }
    }

    /// `lui; daddiu; dsll 16; daddiu; dsll 16; daddiu` rebuilds a 64-bit value
    /// from its four halves, each immediate sign-extended.
    #[test]
    fn mips_halves_rebuild_the_value(raws in any::<[u32; 4]>(), v in value()) {
        let half = |which: Half, raw: u32| -> u64 {
            let field = Field::MipsHalf16(which);
            let raw = u64::from(raw);
            let e = encode(field, raw, v, Isa::Same, 0).expect("a MIPS half never overflows");
            assert_eq!(e & !0xffff, raw & !0xffff, "{which:?} wrote outside its immediate");
            implicit_addend(field, e).expect("a MIPS half keeps its addend") as u64
        };
        let rebuilt = (half(Half::MipsHighest, raws[0]) << 48)
            .wrapping_add(half(Half::MipsHigher, raws[1]) << 32)
            .wrapping_add(half(Half::Ha, raws[2]) << 16)
            .wrapping_add(half(Half::Lo, raws[3]));
        prop_assert_eq!(rebuilt, v);
    }

    #[test]
    fn mips_branch_round_trips(raw in any::<u32>(), v in value()) {
        let raw = u64::from(raw);
        let encoded = encode(Field::MipsPc16, raw, v, Isa::Same, 0);
        prop_assert_eq!(encoded.is_some(), within(v, -(1 << 17), 1 << 17) && v & 3 == 0);
        if let Some(e) = encoded {
            prop_assert_eq!(implicit_addend(Field::MipsPc16, e), Some(v as i64));
            prop_assert_eq!(e & !0xffff, raw & !0xffff);
        }
    }

    #[test]
    fn mips_jump_round_trips(
        raw in any::<u32>(),
        p in any::<u32>(),
        low in 0u64..(1 << 28),
        elsewhere in any::<bool>(),
    ) {
        let (raw, p) = (u64::from(raw), u64::from(p));
        // `J` reaches only the 256 MiB segment of its delay slot.
        let segment = ((p + 4) >> 28) ^ u64::from(elsewhere);
        let v = (segment << 28) | low;
        let encoded = encode(Field::Mips26, raw, v, Isa::Same, p);
        prop_assert_eq!(encoded.is_some(), !elsewhere);
        if let Some(e) = encoded {
            prop_assert_eq!(implicit_addend(Field::Mips26, e), Some((low & !3) as i64));
            prop_assert_eq!(e & !0x03ff_ffff, raw & !0x03ff_ffff);
        }
    }

    #[test]
    fn rela_only_fields_touch_only_their_bits(
        field in rela_only_field(),
        raw in any::<u32>(),
        v in value(),
    ) {
        let raw = u64::from(raw) & (u64::MAX >> (64 - 8 * field.width()));
        prop_assert_eq!(implicit_addend(field, raw), None);
        let encoded = encode(field, raw, v, Isa::Same, 0);
        prop_assert_eq!(encoded.is_some(), rela_only_fits(field, v));
        if let Some(e) = encoded {
            let m = rela_only_mask(field);
            prop_assert_eq!(e & !m, raw & !m);
        }
    }
}
