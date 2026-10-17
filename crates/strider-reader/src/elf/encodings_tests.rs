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

/// A `J` site anywhere, or 1 to 8 bytes below a 256 MiB boundary (the top of
/// the address space included): within 4, the delay slot is past it.
fn jump_site() -> impl Strategy<Value = u64> {
    prop_oneof![
        2 => any::<u64>(),
        1 => (any::<u64>(), 1u64..=8).prop_map(|(x, k)| (x & !0x0fff_ffff).wrapping_sub(k)),
        1 => (1u64..=8).prop_map(u64::wrapping_neg),
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

    /// A `BL` to a Thumb target becomes a `BLX`, whose H bit holds offset
    /// bit 1; a jump there needs a veneer.
    #[test]
    fn arm_branch_into_thumb(call in any::<bool>(), raw in any::<u32>(), v in value()) {
        let raw = u64::from(raw);
        let field = Field::ArmBranch24 { call };
        let encoded = encode(field, raw, v, Isa::Thumb, 0);
        prop_assert_eq!(encoded.is_some(), call && within(v, -(1 << 25), 1 << 25));
        if let Some(e) = encoded {
            prop_assert_eq!(e & 0xfe00_0000, 0xfa00_0000);
            prop_assert_eq!(implicit_addend(field, e), Some((v & !1) as i64));
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

    /// A Thumb `BL` to an ARM target becomes a `BLX`, taken from the PC
    /// rounded down to a word; a jump there needs a veneer.
    #[test]
    fn thumb_branch_into_arm(call in any::<bool>(), raw in any::<u32>(), v in value()) {
        let raw = u64::from(raw);
        let field = Field::ThumbBranch { call };
        let encoded = encode(field, raw, v, Isa::Arm, 0);
        let offset = v.wrapping_add(2) & !3;
        prop_assert_eq!(encoded.is_some(), call && within(offset, -(1 << 24), 1 << 24));
        if let Some(e) = encoded {
            prop_assert_eq!(implicit_addend(field, e), Some(offset as i64));
            prop_assert_eq!(e & 0x1000, 0);
            prop_assert_eq!(e & 0xf800_c000, raw & 0xf800_c000);
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

    /// The plain halves are `v`'s 16-bit slices; the adjusted ones are those
    /// of `v + 0x8000`, which a sign-extended `@l` brings back to `v`.
    #[test]
    fn ppc_halves_rebuild_the_value(v in value()) {
        let half = |which: Half| {
            encode(Field::PpcHalf16(which), 0, v, Isa::Same, 0)
                .expect("an unchecked half never overflows")
        };
        let plain = (half(Half::Highest) << 48)
            | (half(Half::Higher) << 32)
            | (half(Half::Hi) << 16)
            | half(Half::Lo);
        prop_assert_eq!(plain, v);
        let adjusted =
            (half(Half::HighestA) << 48) | (half(Half::HigherA) << 32) | (half(Half::Ha) << 16);
        let signed_lo = half(Half::Lo) as u16 as i16 as u64;
        prop_assert_eq!(adjusted.wrapping_add(signed_lo), v);
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
        p in jump_site(),
        low in 0u64..(1 << 28),
        elsewhere in any::<bool>(),
    ) {
        let raw = u64::from(raw);
        // `J` reaches only the 256 MiB segment of its delay slot.
        let segment = (p.wrapping_add(4) >> 28) ^ u64::from(elsewhere);
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

/// gas's `@ha`, `@highera` and `@highesta`: the plain half of `v + 0x8000`.
#[test]
fn ppc_adjusted_halves_carry_into_the_half_above() {
    use Half::{Ha, Hi, Higher, HigherA, Highest, HighestA};
    for (v, half, want) in [
        (0x0000_0001_ffff_8000, Hi, 0xffff),
        (0x0000_0001_ffff_8000, Ha, 0),
        (0x0000_0001_ffff_8000, Higher, 1),
        (0x0000_0001_ffff_8000, HigherA, 2),
        (0x0000_ffff_ffff_8000, Highest, 0),
        (0x0000_ffff_ffff_8000, HighestA, 1),
        (0xffff_ffff_ffff_8000, HighestA, 0),
        (0x1234_5678_9abc_def0, Ha, 0x9abd),
        (0x1234_5678_9abc_def0, HigherA, 0x5678),
        (0x1234_5678_9abc_def0, HighestA, 0x1234),
    ] {
        assert_eq!(
            encode(Field::PpcHalf16(half), 0, v, Isa::Same, 0),
            Some(want),
            "{half:?} of {v:#x}"
        );
    }
}

/// AArch64 page, low-12 and move-wide immediates, as the assembler encodes
/// them over the instruction with a zero immediate.
#[test]
fn a64_immediates_match_the_assembler() {
    let adrp = Field::A64Adr {
        page: true,
        check: true,
    };
    let adr = Field::A64Adr {
        page: false,
        check: true,
    };
    let movw = |group, check| Field::A64Movw { group, check };
    let cases = [
        // adrp x0, .+0x12345000
        (adrp, 0x9000_0000, 0x1234_5000, 0xb009_1a20),
        // adrp x0, .-0x1000
        (adrp, 0x9000_0000, (-0x1000i64) as u64, 0xf0ff_ffe0),
        // adr x1, .+5
        (adr, 0x1000_0001, 5, 0x3000_0021),
        // add x0, x0, #0xabc
        (
            Field::A64Lo12 { shift: 0 },
            0x9100_0000,
            0x1234_5abc,
            0x912a_f000,
        ),
        // ldr x0, [x1, #0x238]
        (
            Field::A64Lo12 { shift: 3 },
            0xf940_0020,
            0x1234_5238,
            0xf941_1c20,
        ),
        // movz x0, #0x1234, lsl #16
        (movw(1, true), 0xd2a0_0000, 0x1234_5678, 0xd2a2_4680),
        // movk x0, #0xdead, lsl #48
        (
            movw(3, false),
            0xf2e0_0000,
            0xdead_0000_0000_0000,
            0xf2fb_d5a0,
        ),
    ];
    for (field, raw, v, want) in cases {
        assert_eq!(
            encode(field, raw, v, Isa::Same, 0),
            Some(want),
            "{field:?} of {v:#x}"
        );
    }
}

/// The proptest pinning `field`'s round trip, by name. Exhaustive, so a new
/// `Field` does not compile until it names one.
fn round_trip_of(field: Field) -> &'static str {
    match field {
        Field::Data { .. } => "data_round_trips",
        Field::ArmBranch24 { .. } => "arm_branch_round_trips",
        Field::ArmPrel31 => "arm_prel31_round_trips",
        Field::ArmMovw { .. } | Field::ThumbMovw { .. } => "movw_round_trips",
        Field::ThumbBranch { .. } => "thumb_branch_round_trips",
        Field::MipsHalf16(_) => "mips_halves_rebuild_the_value",
        Field::MipsPc16 => "mips_branch_round_trips",
        Field::Mips26 => "mips_jump_round_trips",
        Field::PpcHalf16(_) => "ppc_halves_rebuild_the_value",
        Field::A64Branch26
        | Field::A64Imm19
        | Field::A64Imm14
        | Field::A64Adr { .. }
        | Field::A64Lo12 { .. }
        | Field::A64Movw { .. }
        | Field::PpcBranch24
        | Field::PpcBranch14
        | Field::PpcHalf16Ds(_) => "rela_only_fields_touch_only_their_bits",
    }
}

/// Every field names a round trip this file defines.
#[test]
fn every_field_names_a_round_trip_defined_here() {
    let source = include_str!("encodings_tests.rs");
    for field in [
        Field::Data {
            bytes: 4,
            signed: false,
        },
        Field::A64Branch26,
        Field::A64Imm19,
        Field::A64Imm14,
        Field::A64Adr {
            page: true,
            check: true,
        },
        Field::A64Lo12 { shift: 0 },
        Field::A64Movw {
            group: 0,
            check: true,
        },
        Field::ArmBranch24 { call: true },
        Field::ArmPrel31,
        Field::ArmMovw { top: false },
        Field::ThumbBranch { call: true },
        Field::ThumbMovw { top: false },
        Field::PpcBranch24,
        Field::PpcBranch14,
        Field::PpcHalf16(Half::Lo),
        Field::PpcHalf16Ds(Half::Lo),
        Field::Mips26,
        Field::MipsHalf16(Half::Lo),
        Field::MipsPc16,
    ] {
        let name = round_trip_of(field);
        assert!(
            source.contains(&format!("fn {name}(")),
            "{field:?} names {name}, which this file does not define"
        );
    }
}
