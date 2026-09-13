//! Relocation types whose value `object` does not describe: instruction
//! immediates, GOT and TOC references, and the few plain fields it leaves
//! `Unknown`. Each is classified into the value it computes and the field that
//! value is encoded into.
//!
//! Formulas are the psABI ones: `S` the symbol, `A` the addend, `P` the site,
//! `G` the symbol's GOT slot, `GOT` the GOT's start and `TOC` the PowerPC64
//! TOC pointer.

use object::Architecture as A;
use object::elf;

/// Types `object` does not name.
const R_AARCH64_PLT32: u32 = 314;
const R_PPC64_REL24_NOTOC: u32 = 116;
const R_PPC64_ENTRY: u32 = 118;

/// What a relocation computes, before it is encoded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Value {
    /// `S + A`
    Abs,
    /// `S + A - P`
    Pcrel,
    /// `Page(S + A) - Page(P)`
    PagePcrel,
    /// `G + A - P`
    GotPcrel,
    /// `Page(G) - Page(P)`
    GotPagePcrel,
    /// `G`
    Got,
    /// `G - GOT + A`
    GotOffset,
    /// `S + A - GOT`
    GotRelative,
    /// `GOT + A - P`
    GotBasePcrel,
    /// `S + A - TOC`
    TocRelative,
    /// `TOC + A`
    Toc,
}

/// Which bits of a 16-bit PowerPC or MIPS immediate a value lands in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Half {
    /// All 16 bits, signed or unsigned.
    Whole,
    /// Signed 16 bits.
    Signed,
    Lo,
    Hi,
    /// `Hi`, adjusted for a signed `Lo`.
    Ha,
    Higher,
    /// `Higher`, adjusted for a signed `Lo` below it.
    HigherA,
    Highest,
    HighestA,
    /// MIPS `%higher`, adjusted for signed `%hi` and `%lo` below it.
    MipsHigher,
    /// MIPS `%highest`.
    MipsHighest,
}

/// Where the value lands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Field {
    /// Plain `bytes`-wide data. `signed` rejects a value outside the signed
    /// range; otherwise either range is accepted.
    Data { bytes: u8, signed: bool },
    /// AArch64 `B` / `BL`: `imm26 = v >> 2`.
    A64Branch26,
    /// AArch64 `B.cond` / literal load: `imm19 = v >> 2` at bit 5.
    A64Imm19,
    /// AArch64 `TBZ`: `imm14 = v >> 2` at bit 5.
    A64Imm14,
    /// AArch64 `ADR` / `ADRP`: a 21-bit `immhi:immlo`, `v >> 12` for a page.
    A64Adr { page: bool, check: bool },
    /// AArch64 `ADD` / `LDR` `imm12 = (v & 0xfff) >> shift` at bit 10.
    A64Lo12 { shift: u8 },
    /// AArch64 `MOVZ` / `MOVK` `imm16 = v >> (16 * group)` at bit 5.
    A64Movw { group: u8, check: bool },
    /// ARM `B` / `BL` / `BLX`: `imm24 = v >> 2`; `call` may switch `BL` and
    /// `BLX` for an interworking target.
    ArmBranch24 { call: bool },
    /// ARM `PREL31`: the low 31 bits of a word.
    ArmPrel31,
    /// ARM `MOVW` / `MOVT` `imm4:imm12`.
    ArmMovw { top: bool },
    /// Thumb-2 `BL` / `BLX` / `B.W`, two halfwords.
    ThumbBranch { call: bool },
    /// Thumb-2 `MOVW` / `MOVT` `imm4:i:imm3:imm8`.
    ThumbMovw { top: bool },
    /// PowerPC `B` / `BL` `LI` field, 24 bits at bit 2.
    PpcBranch24,
    /// PowerPC `BC` `BD` field, 14 bits at bit 2.
    PpcBranch14,
    /// A PowerPC 16-bit field addressed directly by `r_offset`.
    PpcHalf16(Half),
    /// MIPS `J` / `JAL` `instr_index`.
    Mips26,
    /// MIPS 16-bit immediate, the low half of the instruction word.
    MipsHalf16(Half),
    /// MIPS branch offset, `v >> 2` in the low 16 bits.
    MipsPc16,
}

impl Field {
    /// Bytes at `r_offset` the field reads and patches.
    pub(crate) fn width(self) -> usize {
        match self {
            Field::Data { bytes, .. } => usize::from(bytes),
            Field::PpcHalf16(_) => 2,
            _ => 4,
        }
    }
}

/// How a relocation type is applied.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    /// Leaves the field as assembled: `R_*_NONE` and pure markers.
    Marker,
    Compute(Value, Field),
    /// PowerPC64 `REL24`, which enters a local function past its TOC setup
    /// and follows an ELFv1 descriptor.
    Ppc64Call {
        notoc: bool,
    },
    /// PowerPC `PLTREL24`, `S - P`: a `-fPIC` addend locates the caller's GOT
    /// pointer, not the target.
    PpcPltCall,
}

/// Classifies `r_type` for `arch`, `None` when it is not one of these.
///
/// MIPS takes the relocation's first type with the other two `R_MIPS_NONE`;
/// the caller rejects a real composite.
pub(crate) fn classify(arch: A, r_type: u32) -> Option<Kind> {
    use Field as F;
    use Kind::{Compute as C, Marker};
    use Value as V;
    let data = |bytes: u8, signed: bool| F::Data { bytes, signed };
    Some(match arch {
        A::X86_64 => match r_type {
            elf::R_X86_64_NONE | elf::R_X86_64_TLSDESC_CALL => Marker,
            elf::R_X86_64_GOTPCREL | elf::R_X86_64_GOTPCRELX | elf::R_X86_64_REX_GOTPCRELX => {
                C(V::GotPcrel, data(4, true))
            }
            elf::R_X86_64_GOTPCREL64 => C(V::GotPcrel, data(8, false)),
            elf::R_X86_64_GOTPC32 => C(V::GotBasePcrel, data(4, true)),
            elf::R_X86_64_GOTPC64 => C(V::GotBasePcrel, data(8, false)),
            elf::R_X86_64_GOTOFF64 => C(V::GotRelative, data(8, false)),
            elf::R_X86_64_PC64 => C(V::Pcrel, data(8, false)),
            _ => return None,
        },
        A::I386 => match r_type {
            elf::R_386_NONE | elf::R_386_TLS_DESC_CALL => Marker,
            elf::R_386_GOT32 | elf::R_386_GOT32X => C(V::GotOffset, data(4, false)),
            elf::R_386_GOTOFF => C(V::GotRelative, data(4, false)),
            elf::R_386_GOTPC => C(V::GotBasePcrel, data(4, false)),
            _ => return None,
        },
        A::Aarch64 => match r_type {
            elf::R_AARCH64_NONE | 256 | elf::R_AARCH64_TLSDESC_CALL => Marker,
            elf::R_AARCH64_CALL26 | elf::R_AARCH64_JUMP26 => C(V::Pcrel, F::A64Branch26),
            elf::R_AARCH64_CONDBR19 | elf::R_AARCH64_LD_PREL_LO19 => C(V::Pcrel, F::A64Imm19),
            elf::R_AARCH64_GOT_LD_PREL19 => C(V::GotPcrel, F::A64Imm19),
            elf::R_AARCH64_TSTBR14 => C(V::Pcrel, F::A64Imm14),
            elf::R_AARCH64_ADR_PREL_LO21 => C(
                V::Pcrel,
                F::A64Adr {
                    page: false,
                    check: true,
                },
            ),
            elf::R_AARCH64_ADR_PREL_PG_HI21 => C(
                V::PagePcrel,
                F::A64Adr {
                    page: true,
                    check: true,
                },
            ),
            elf::R_AARCH64_ADR_PREL_PG_HI21_NC => C(
                V::PagePcrel,
                F::A64Adr {
                    page: true,
                    check: false,
                },
            ),
            elf::R_AARCH64_ADR_GOT_PAGE => C(
                V::GotPagePcrel,
                F::A64Adr {
                    page: true,
                    check: true,
                },
            ),
            elf::R_AARCH64_ADD_ABS_LO12_NC | elf::R_AARCH64_LDST8_ABS_LO12_NC => {
                C(V::Abs, F::A64Lo12 { shift: 0 })
            }
            elf::R_AARCH64_LDST16_ABS_LO12_NC => C(V::Abs, F::A64Lo12 { shift: 1 }),
            elf::R_AARCH64_LDST32_ABS_LO12_NC => C(V::Abs, F::A64Lo12 { shift: 2 }),
            elf::R_AARCH64_LDST64_ABS_LO12_NC => C(V::Abs, F::A64Lo12 { shift: 3 }),
            elf::R_AARCH64_LDST128_ABS_LO12_NC => C(V::Abs, F::A64Lo12 { shift: 4 }),
            elf::R_AARCH64_LD64_GOT_LO12_NC => C(V::Got, F::A64Lo12 { shift: 3 }),
            elf::R_AARCH64_MOVW_UABS_G0 => C(V::Abs, movw(0, true)),
            elf::R_AARCH64_MOVW_UABS_G0_NC => C(V::Abs, movw(0, false)),
            elf::R_AARCH64_MOVW_UABS_G1 => C(V::Abs, movw(1, true)),
            elf::R_AARCH64_MOVW_UABS_G1_NC => C(V::Abs, movw(1, false)),
            elf::R_AARCH64_MOVW_UABS_G2 => C(V::Abs, movw(2, true)),
            elf::R_AARCH64_MOVW_UABS_G2_NC => C(V::Abs, movw(2, false)),
            elf::R_AARCH64_MOVW_UABS_G3 => C(V::Abs, movw(3, false)),
            R_AARCH64_PLT32 => C(V::Pcrel, data(4, true)),
            _ => return None,
        },
        A::Arm => match r_type {
            elf::R_ARM_NONE | elf::R_ARM_V4BX => Marker,
            elf::R_ARM_CALL => C(V::Pcrel, F::ArmBranch24 { call: true }),
            elf::R_ARM_PC24 | elf::R_ARM_JUMP24 | elf::R_ARM_PLT32 => {
                C(V::Pcrel, F::ArmBranch24 { call: false })
            }
            elf::R_ARM_THM_PC22 => C(V::Pcrel, F::ThumbBranch { call: true }),
            elf::R_ARM_THM_JUMP24 => C(V::Pcrel, F::ThumbBranch { call: false }),
            elf::R_ARM_PREL31 => C(V::Pcrel, F::ArmPrel31),
            elf::R_ARM_MOVW_ABS_NC => C(V::Abs, F::ArmMovw { top: false }),
            elf::R_ARM_MOVT_ABS => C(V::Abs, F::ArmMovw { top: true }),
            elf::R_ARM_MOVW_PREL_NC => C(V::Pcrel, F::ArmMovw { top: false }),
            elf::R_ARM_MOVT_PREL => C(V::Pcrel, F::ArmMovw { top: true }),
            elf::R_ARM_THM_MOVW_ABS_NC => C(V::Abs, F::ThumbMovw { top: false }),
            elf::R_ARM_THM_MOVT_ABS => C(V::Abs, F::ThumbMovw { top: true }),
            elf::R_ARM_THM_MOVW_PREL_NC => C(V::Pcrel, F::ThumbMovw { top: false }),
            elf::R_ARM_THM_MOVT_PREL => C(V::Pcrel, F::ThumbMovw { top: true }),
            // Linux's `ld` default, `--target1-abs`.
            elf::R_ARM_TARGET1 => C(V::Abs, data(4, false)),
            elf::R_ARM_GOT32 => C(V::GotOffset, data(4, false)),
            elf::R_ARM_GOT_PREL => C(V::GotPcrel, data(4, false)),
            elf::R_ARM_GOTOFF => C(V::GotRelative, data(4, false)),
            elf::R_ARM_GOTPC => C(V::GotBasePcrel, data(4, false)),
            _ => return None,
        },
        A::PowerPc | A::PowerPc64 => {
            let ppc64 = arch == A::PowerPc64;
            match r_type {
                elf::R_PPC_NONE => Marker,
                elf::R_PPC64_TLS | elf::R_PPC64_TOCSAVE | R_PPC64_ENTRY if ppc64 => Marker,
                elf::R_PPC64_REL24 if ppc64 => Kind::Ppc64Call { notoc: false },
                R_PPC64_REL24_NOTOC if ppc64 => Kind::Ppc64Call { notoc: true },
                elf::R_PPC_REL24 | elf::R_PPC_LOCAL24PC => C(V::Pcrel, F::PpcBranch24),
                elf::R_PPC_PLTREL24 if !ppc64 => Kind::PpcPltCall,
                elf::R_PPC_REL14 | elf::R_PPC_REL14_BRTAKEN | elf::R_PPC_REL14_BRNTAKEN => {
                    C(V::Pcrel, F::PpcBranch14)
                }
                elf::R_PPC_ADDR16 => C(V::Abs, F::PpcHalf16(Half::Whole)),
                elf::R_PPC_ADDR16_LO => C(V::Abs, F::PpcHalf16(Half::Lo)),
                elf::R_PPC_ADDR16_HI => C(V::Abs, F::PpcHalf16(Half::Hi)),
                elf::R_PPC_ADDR16_HA => C(V::Abs, F::PpcHalf16(Half::Ha)),
                elf::R_PPC_REL16 => C(V::Pcrel, F::PpcHalf16(Half::Signed)),
                elf::R_PPC_REL16_LO => C(V::Pcrel, F::PpcHalf16(Half::Lo)),
                elf::R_PPC_REL16_HI => C(V::Pcrel, F::PpcHalf16(Half::Hi)),
                elf::R_PPC_REL16_HA => C(V::Pcrel, F::PpcHalf16(Half::Ha)),
                elf::R_PPC64_ADDR16_DS if ppc64 => C(V::Abs, F::PpcHalf16(Half::Signed)),
                elf::R_PPC64_ADDR16_LO_DS if ppc64 => C(V::Abs, F::PpcHalf16(Half::Lo)),
                elf::R_PPC64_ADDR16_HIGH if ppc64 => C(V::Abs, F::PpcHalf16(Half::Hi)),
                elf::R_PPC64_ADDR16_HIGHA if ppc64 => C(V::Abs, F::PpcHalf16(Half::Ha)),
                elf::R_PPC64_ADDR16_HIGHER if ppc64 => C(V::Abs, F::PpcHalf16(Half::Higher)),
                elf::R_PPC64_ADDR16_HIGHERA if ppc64 => C(V::Abs, F::PpcHalf16(Half::HigherA)),
                elf::R_PPC64_ADDR16_HIGHEST if ppc64 => C(V::Abs, F::PpcHalf16(Half::Highest)),
                elf::R_PPC64_ADDR16_HIGHESTA if ppc64 => C(V::Abs, F::PpcHalf16(Half::HighestA)),
                elf::R_PPC64_TOC16 | elf::R_PPC64_TOC16_DS if ppc64 => {
                    C(V::TocRelative, F::PpcHalf16(Half::Signed))
                }
                elf::R_PPC64_TOC16_LO | elf::R_PPC64_TOC16_LO_DS if ppc64 => {
                    C(V::TocRelative, F::PpcHalf16(Half::Lo))
                }
                elf::R_PPC64_TOC16_HI if ppc64 => C(V::TocRelative, F::PpcHalf16(Half::Hi)),
                elf::R_PPC64_TOC16_HA if ppc64 => C(V::TocRelative, F::PpcHalf16(Half::Ha)),
                elf::R_PPC64_TOC if ppc64 => C(V::Toc, data(8, false)),
                _ => return None,
            }
        }
        A::Mips | A::Mips64 => match r_type {
            elf::R_MIPS_NONE | elf::R_MIPS_JALR => Marker,
            elf::R_MIPS_26 => C(V::Abs, F::Mips26),
            elf::R_MIPS_HI16 => C(V::Abs, F::MipsHalf16(Half::Ha)),
            elf::R_MIPS_LO16 => C(V::Abs, F::MipsHalf16(Half::Lo)),
            elf::R_MIPS_HIGHER => C(V::Abs, F::MipsHalf16(Half::MipsHigher)),
            elf::R_MIPS_HIGHEST => C(V::Abs, F::MipsHalf16(Half::MipsHighest)),
            elf::R_MIPS_PC16 => C(V::Pcrel, F::MipsPc16),
            _ => return None,
        },
        _ => return None,
    })
}

fn movw(group: u8, check: bool) -> Field {
    Field::A64Movw { group, check }
}

/// Sign-extends the low `bits` of `v`.
pub(crate) fn sext(v: u64, bits: u32) -> i64 {
    let shift = 64 - bits;
    ((v << shift) as i64) >> shift
}

fn fits_signed(v: i64, bits: u32) -> bool {
    bits >= 64 || (-(1i64 << (bits - 1))..(1i64 << (bits - 1))).contains(&v)
}

fn fits_either(v: u64, bits: u32) -> bool {
    bits >= 64 || fits_signed(v as i64, bits) || v >> bits == 0
}

/// The halfwords of a Thumb-2 instruction read as one word, first halfword
/// high.
fn halfwords(raw: u64) -> (u64, u64) {
    ((raw >> 16) & 0xffff, raw & 0xffff)
}

/// The implicit addend an `SHT_REL` relocation keeps in the field, from the
/// field's file-initial `raw` bits; `None` for a field with no addend slot.
pub(crate) fn implicit_addend(field: Field, raw: u64) -> Option<i64> {
    Some(match field {
        Field::Data { bytes, .. } => sext(raw, 8 * u32::from(bytes)),
        Field::ArmBranch24 { .. } => {
            // A `BLX`'s H bit is the offset's bit 1.
            let h = if raw >> 28 == 0xf { (raw >> 24) & 1 } else { 0 };
            sext(((raw & 0x00ff_ffff) << 2) | (h << 1), 26)
        }
        Field::ArmPrel31 => sext(raw & 0x7fff_ffff, 31),
        Field::ArmMovw { .. } => sext(((raw >> 4) & 0xf000) | (raw & 0xfff), 16),
        Field::ThumbBranch { .. } => {
            let (hw1, hw2) = halfwords(raw);
            let s = (hw1 >> 10) & 1;
            let i1 = !((hw2 >> 13) ^ s) & 1;
            let i2 = !((hw2 >> 11) ^ s) & 1;
            let off =
                (s << 24) | (i1 << 23) | (i2 << 22) | ((hw1 & 0x3ff) << 12) | ((hw2 & 0x7ff) << 1);
            sext(off, 25)
        }
        Field::ThumbMovw { .. } => {
            let (hw1, hw2) = halfwords(raw);
            let imm = ((hw1 & 0xf) << 12)
                | (((hw1 >> 10) & 1) << 11)
                | (((hw2 >> 12) & 7) << 8)
                | (hw2 & 0xff);
            sext(imm, 16)
        }
        Field::MipsHalf16(_) => sext(raw & 0xffff, 16),
        Field::MipsPc16 => sext((raw & 0xffff) << 2, 18),
        // `R_MIPS_26`'s addend depends on the symbol's binding; see `encode`.
        Field::Mips26 => ((raw & 0x03ff_ffff) << 2) as i64,
        _ => return None,
    })
}

/// How a branch's target is entered, for interworking.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Isa {
    /// The target's ISA is not recorded: stay in the caller's.
    Same,
    Arm,
    Thumb,
}

/// `value` encoded into `field` over its file-initial `raw` bits, or `None`
/// when it does not fit or needs a veneer the link would add.
///
/// `target` is the branch target's ISA; `p` the site.
pub(crate) fn encode(field: Field, raw: u64, value: u64, target: Isa, p: u64) -> Option<u64> {
    let v = value as i64;
    Some(match field {
        Field::Data { bytes, signed } => {
            let bits = 8 * u32::from(bytes);
            let ok = if signed {
                fits_signed(v, bits)
            } else {
                fits_either(value, bits)
            };
            if !ok {
                return None;
            }
            value
        }
        Field::A64Branch26 => {
            let imm = v >> 2;
            fits_signed(imm, 26).then_some(())?;
            (raw & !0x03ff_ffff) | (imm as u64 & 0x03ff_ffff)
        }
        Field::A64Imm19 => {
            let imm = v >> 2;
            fits_signed(imm, 19).then_some(())?;
            (raw & !(0x7_ffff << 5)) | ((imm as u64 & 0x7_ffff) << 5)
        }
        Field::A64Imm14 => {
            let imm = v >> 2;
            fits_signed(imm, 14).then_some(())?;
            (raw & !(0x3fff << 5)) | ((imm as u64 & 0x3fff) << 5)
        }
        Field::A64Adr { page, check } => {
            let imm = if page { v >> 12 } else { v };
            if check && !fits_signed(imm, 21) {
                return None;
            }
            let imm = imm as u64;
            (raw & !((0x3 << 29) | (0x7_ffff << 5)))
                | ((imm & 3) << 29)
                | (((imm >> 2) & 0x7_ffff) << 5)
        }
        Field::A64Lo12 { shift } => (raw & !(0xfff << 10)) | (((value & 0xfff) >> shift) << 10),
        Field::A64Movw { group, check } => {
            let shift = 16 * u32::from(group);
            if check && value >> (shift + 16) != 0 {
                return None;
            }
            (raw & !(0xffff << 5)) | (((value >> shift) & 0xffff) << 5)
        }
        Field::ArmBranch24 { call } => {
            fits_signed(v, 26).then_some(())?;
            let imm = (value >> 2) & 0x00ff_ffff;
            match (target, call) {
                (Isa::Thumb, true) => 0xfa00_0000 | (((value >> 1) & 1) << 24) | imm,
                // A jump into Thumb needs a veneer.
                (Isa::Thumb, false) => return None,
                // An ARM target reached by a `BLX` becomes a `BL`.
                (_, true) if raw >> 28 == 0xf => 0xeb00_0000 | imm,
                _ => (raw & 0xff00_0000) | imm,
            }
        }
        Field::ArmPrel31 => {
            fits_signed(v, 31).then_some(())?;
            (raw & 0x8000_0000) | (value & 0x7fff_ffff)
        }
        Field::ArmMovw { top } => {
            let imm = if top { value >> 16 } else { value } & 0xffff;
            (raw & !0x000f_0fff) | ((imm >> 12) << 16) | (imm & 0xfff)
        }
        Field::ThumbBranch { call } => {
            let (hw1, hw2) = halfwords(raw);
            // `BLX` lands on a word boundary of the Thumb PC.
            let (v, blx) = match (target, call) {
                (Isa::Arm, true) => (((value + 2) & !3) as i64, true),
                (Isa::Arm, false) => return None,
                _ => (v, false),
            };
            fits_signed(v, 25).then_some(())?;
            let off = v as u64;
            let s = (off >> 24) & 1;
            let j1 = (!(off >> 23) ^ s) & 1;
            let j2 = (!(off >> 22) ^ s) & 1;
            let hw1 = (hw1 & 0xf800) | (s << 10) | ((off >> 12) & 0x3ff);
            let mut hw2 = (hw2 & 0xd000) | (j1 << 13) | (j2 << 11) | ((off >> 1) & 0x7ff);
            if call {
                hw2 = if blx { hw2 & !0x1000 } else { hw2 | 0x1000 };
            }
            (hw1 << 16) | hw2
        }
        Field::ThumbMovw { top } => {
            let (hw1, hw2) = halfwords(raw);
            let imm = if top { value >> 16 } else { value } & 0xffff;
            let hw1 = (hw1 & !0x040f) | ((imm >> 12) & 0xf) | (((imm >> 11) & 1) << 10);
            let hw2 = (hw2 & !0x70ff) | (((imm >> 8) & 7) << 12) | (imm & 0xff);
            (hw1 << 16) | hw2
        }
        Field::PpcBranch24 => {
            (fits_signed(v, 26) && v & 3 == 0).then_some(())?;
            (raw & !0x03ff_fffc) | (value & 0x03ff_fffc)
        }
        Field::PpcBranch14 => {
            (fits_signed(v, 16) && v & 3 == 0).then_some(())?;
            (raw & !0xfffc) | (value & 0xfffc)
        }
        Field::PpcHalf16(half) => half16(half, value)?,
        Field::MipsHalf16(half) => (raw & !0xffff) | half16(half, value)?,
        Field::Mips26 => {
            // `J` keeps the top four bits of the delay slot's address.
            ((value >> 28) == (p.wrapping_add(4) >> 28)).then_some(())?;
            (raw & !0x03ff_ffff) | ((value >> 2) & 0x03ff_ffff)
        }
        Field::MipsPc16 => {
            (fits_signed(v, 18) && v & 3 == 0).then_some(())?;
            (raw & !0xffff) | ((value >> 2) & 0xffff)
        }
    })
}

/// The 16 bits `half` selects from `value`, `None` on overflow.
fn half16(half: Half, value: u64) -> Option<u64> {
    let v = match half {
        Half::Whole => fits_either(value, 16).then_some(value)?,
        Half::Signed => fits_signed(value as i64, 16).then_some(value)?,
        Half::Lo => value,
        Half::Hi => value >> 16,
        Half::Ha => value.wrapping_add(0x8000) >> 16,
        Half::Higher => value >> 32,
        Half::HigherA => value.wrapping_add(0x8000) >> 32,
        Half::Highest => value >> 48,
        Half::HighestA => value.wrapping_add(0x8000) >> 48,
        Half::MipsHigher => value.wrapping_add(0x8000_8000) >> 32,
        Half::MipsHighest => value.wrapping_add(0x8000_8000_8000) >> 48,
    };
    Some(v & 0xffff)
}
