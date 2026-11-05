use object::{
    Architecture as A, Object, ObjectSection, ObjectSymbol, ObjectSymbolTable, RelocationKind,
};

use crate::MemRegion;
use crate::elf::encodings::{
    Field, Half, Isa, Kind, Value, classify, encode, implicit_addend, sext,
};
use crate::elf::sections::is_ifunc;

use super::implicit::note_copy;
use super::walk::RelocInfo;
use super::{Ctx, RelocSite};

/// `base + addend`, modular: `Patch::new` truncates to the field width.
#[inline]
pub(super) fn apply_addend(base: u64, addend: i64) -> u64 {
    base.wrapping_add(addend as u64)
}

/// Applies one relocation entry at the already-absolute `site.addr`: the row
/// [`classify`] names, else the plain field `object` describes, else a hole.
pub(super) fn apply_one_relocation(
    ctx: &mut Ctx<'_, '_>,
    site: RelocSite,
    r: &RelocInfo,
    pair: Option<u64>,
) {
    match classify(ctx.obj.architecture(), r) {
        Some(Kind::Marker) => {}
        Some(Kind::Compute(value, field)) => apply_classified(ctx, site, r, pair, value, field),
        Some(Kind::Copy) => {
            let size = r.sym.and_then(|idx| symbol_size(ctx.obj, idx));
            note_copy(ctx, site.addr, size, r.type_word());
        }
        None => apply_plain(ctx, site, r),
    }
}

/// `st_size` of symbol `idx`.
pub(super) fn symbol_size(obj: &object::File<'_>, idx: object::SymbolIndex) -> Option<u64> {
    Some(reloc_symbol(obj, idx)?.size())
}

/// The symbol a relocation's `r_sym` names: `.symtab`'s in an ET_REL,
/// `.dynsym`'s in a linked image (`None` without one). `object`'s
/// `dynamic_symbol_table` is any `SHT_DYNSYM`, even an ET_REL's.
fn reloc_symbol<'d, 'f>(
    obj: &'f object::File<'d>,
    idx: object::SymbolIndex,
) -> Option<object::Symbol<'d, 'f>> {
    if obj.kind() == object::ObjectKind::Relocatable {
        obj.symbol_by_index(idx)
    } else {
        obj.dynamic_symbol_table()?.symbol_by_index(idx)
    }
    .ok()
}

/// A plain data field `object` describes: `Absolute` is `S + A`, `Relative`
/// and `PltRelative` `S + A - P` (no PLT, so L is S). Anything else is a hole.
pub(super) fn apply_plain(ctx: &mut Ctx<'_, '_>, site: RelocSite, r: &RelocInfo) {
    let obj = ctx.obj;
    let site_addr = site.addr;
    let endian_le = ctx.endian_le;

    // A mips64el implicit-addend entry, whose kind and size `object` may
    // have read off its symbol index.
    let Some(generic) = r.generic else {
        let width = unknown_field_bytes(obj.architecture(), r);
        let holder = ctx.holder_or_hole(site, site_addr, width, r.type_word());
        ctx.unmodelled(holder, site_addr, width, r.type_word());
        return;
    };
    // `size` is in bits; 0 marks an arch-specific encoding, not modelled.
    let size_bits = generic.size_bits;
    let plain = size_bits != 0
        && size_bits.is_multiple_of(8)
        && size_bits <= 64
        // A non-`Generic` encoding is not plain low bytes (s390x `*DBL` is
        // `(S + A - P) >> 1`), except `X86Signed`, which only names a sign
        // extension.
        && matches!(
            generic.encoding,
            object::RelocationEncoding::Generic | object::RelocationEncoding::X86Signed
        );
    let pc_relative = match generic.kind {
        RelocationKind::Absolute => Some(false),
        RelocationKind::Relative | RelocationKind::PltRelative => Some(true),
        _ => None,
    };
    let (Some(pc_relative), true) = (pc_relative, plain) else {
        let width = if plain {
            (size_bits / 8) as usize
        } else {
            unknown_field_bytes(obj.architecture(), r)
        };
        let holder = ctx.holder_or_hole(site, site_addr, width, r.type_word());
        ctx.unmodelled(holder, site_addr, width, r.type_word());
        return;
    };
    let size_bytes = (size_bits / 8) as usize;

    // `r_offset` addresses the storage unit; the field can sit inside it.
    let skew = mips_half_field_skew(r, obj.architecture(), endian_le);
    let Some(field_addr) = site_addr.checked_add(skew) else {
        return;
    };

    let holder = ctx.holder_or_hole(site, field_addr, size_bytes, r.type_word());
    // No `S`: STN_UNDEF, or a symbol with no address.
    let Some(target) = r
        .sym
        .and_then(|idx| resolve_symbol_target(obj, ctx.layout, idx))
    else {
        ctx.unmodelled(holder, field_addr, size_bytes, r.type_word());
        return;
    };
    // Only a call's displacement hides the address, see
    // `Target::weak_undefined`.
    if (target.ifunc && !ctx.in_code(field_addr, size_bytes))
        || (target.weak_undefined && !matches!(generic.kind, RelocationKind::PltRelative))
    {
        ctx.unmodelled(holder, field_addr, size_bytes, r.type_word());
        return;
    }
    let field = Field::Data {
        bytes: size_bytes as u8,
        signed: pc_relative || matches!(generic.encoding, object::RelocationEncoding::X86Signed),
    };
    // `SHT_REL` keeps A in the field, sign-extended (-4 for x86 `call
    // rel32`). A field no region holds reads 0 and is not patched.
    let addend = match r.rela_addend {
        Some(addend) => addend,
        None => holder
            .and_then(|region| {
                implicit_addend(field, read_field(region, field_addr, size_bytes, endian_le))
            })
            .unwrap_or(0),
    };
    // P is the storage unit, not the field inside it.
    let value = apply_addend(target.addr, addend);
    let value = if pc_relative {
        value.wrapping_sub(site_addr)
    } else {
        value
    };
    // `R_X86_64_32`'s value zero-extends (x86-64 psABI 4.4).
    let zero_extends = obj.architecture() == A::X86_64
        && !pc_relative
        && size_bytes == 4
        && matches!(generic.encoding, object::RelocationEncoding::Generic);
    match fit(obj, field, value).filter(|v| !zero_extends || v >> 32 == 0) {
        Some(value) => ctx.record(holder, field_addr, value, size_bytes, endian_le),
        None => ctx.unmodelled(holder, field_addr, size_bytes, r.type_word()),
    }
}

/// Field width of a relocation `object` does not size: 8 for a MIPS64
/// composite with `R_MIPS_64`, a ppc64 doubleword64 or prefixed instruction,
/// or an x86-64 64-bit word; 2 for a PowerPC half16 (`r_offset` names the
/// halfword); else 4.
pub(super) fn unknown_field_bytes(arch: A, r: &RelocInfo) -> usize {
    match arch {
        A::Mips64 if [r.r_type, r.r_type2, r.r_type3].contains(&object::elf::R_MIPS_64) => 8,
        // The `*16*` types; ppc32 and ppc64 number some apart.
        A::PowerPc => match r.r_type {
            3..=6
            | 14..=17
            | 25
            | 29..=36
            | 69..=72
            | 74..=77
            | 79..=94
            | 102..=108
            | 111..=114
            | 116
            | 249..=252
            | 255 => 2,
            _ => 4,
        },
        A::PowerPc64 => match r.r_type {
            3..=6
            | 14..=17
            | 25
            | 29..=31
            | 33..=36
            | 39..=42
            | 47..=50
            | 52..=66
            | 69..=72
            | 74..=77
            | 79..=106
            | 110..=115
            | 136..=143
            | 240..=245
            | 249..=252 => 2,
            // UADDR64, PLT64, PLTREL64, ADDR64_LOCAL, the prefixed
            // `*34` / `*28` types.
            43 | 45 | 46 | 117 | 128..=135 | 144..=151 => 8,
            _ => 4,
        },
        A::X86_64 => match r.r_type {
            object::elf::R_X86_64_GOTPLT64 | object::elf::R_X86_64_SIZE64 => 8,
            _ => 4,
        },
        _ => 4,
    }
}

/// `value` checked against a plain data `field`. ELF32 arithmetic wraps at
/// 32 bits: a word always fits, a narrower field is checked sign-extended.
pub(super) fn fit(obj: &object::File<'_>, field: Field, value: u64) -> Option<u64> {
    if obj.is_64() {
        return encode(field, 0, value, Isa::Same, 0);
    }
    let value = i64::from(value as u32 as i32) as u64;
    if field.width() >= 4 {
        return Some(value);
    }
    encode(field, 0, value, Isa::Same, 0)
}

/// A [`classify`]-listed relocation, see [`encodings`](crate::elf::encodings).
pub(super) fn apply_classified(
    ctx: &mut Ctx<'_, '_>,
    site: RelocSite,
    r: &RelocInfo,
    pair: Option<u64>,
    value: Value,
    field: Field,
) {
    // An `SHT_REL` image-relative field already holds `B + A`, `B` being 0.
    if value == Value::ImageRelative && r.rela_addend.is_none() {
        return;
    }
    let p = site.addr;
    let width = field.width();
    let holder = ctx.holder_or_hole(site, p, width, r.type_word());
    // Read nothing: a TLS descriptor is wider than a field read.
    if value == Value::Tls {
        ctx.unmodelled(holder, p, width, r.type_word());
        return;
    }
    // Fast path: a RELA image-relative word reads no symbol or field.
    if let (Value::ImageRelative, Some(a)) = (value, r.rela_addend) {
        match fit(ctx.obj, field, apply_addend(0, a)) {
            Some(word) => ctx.record(holder, p, word, width, ctx.field_le(field)),
            None => ctx.unmodelled(holder, p, width, r.type_word()),
        }
        return;
    }
    let field_le = ctx.field_le(field);
    match compute_classified(ctx, r, pair, value, field, p, holder) {
        Some(word) => ctx.record(holder, p, word, width, field_le),
        None => ctx.unmodelled(holder, p, width, r.type_word()),
    }
}

/// The patched field of a [`classify`]-listed relocation at `p`, `None` when
/// it is not computed.
pub(super) fn compute_classified(
    ctx: &mut Ctx<'_, '_>,
    r: &RelocInfo,
    pair: Option<u64>,
    value_kind: Value,
    field: Field,
    p: u64,
    holder: Option<&MemRegion>,
) -> Option<u64> {
    let (obj, layout) = (ctx.obj, ctx.layout);
    let field_le = ctx.field_le(field);
    let thumb_le = field_le && matches!(field, Field::ThumbBranch { .. } | Field::ThumbMovw { .. });
    let raw = read_field(holder?, p, field.width(), field_le);
    let raw = if thumb_le { swap_halfwords(raw) } else { raw };
    // `S` exists only for a symbol target, and only a value reading it asks.
    let target = match (value_kind.reads_symbol(), r.sym) {
        (true, Some(idx)) => Some(resolve_symbol_target(obj, layout, idx)?),
        (true, _) => return None,
        (false, _) => None,
    };
    // Only a branch or GOT reference hides the address, see
    // `Target::weak_undefined`.
    if target.is_some_and(|t| t.weak_undefined)
        && !field.is_branch()
        && !value_kind_through_got(value_kind)
    {
        return None;
    }
    // Outside code an IFUNC's value is unknown; a GOT reference needs only
    // its slot's address.
    if target.is_some_and(|t| t.ifunc)
        && !value_kind_through_got(value_kind)
        && !ctx.in_code(p, field.width())
    {
        return None;
    }
    let s = target.map_or(0, |t| t.addr);

    let mut a = match (value_kind, r.rela_addend) {
        (_, Some(a)) => a,
        // A slot's `SHT_REL` field holds the PLT push address, not an addend.
        (Value::Slot, None) => 0,
        (_, None) => implicit_addend(field, raw)?,
    };
    match field {
        // The low half of an `SHT_REL` HI16's addend is its LO16's field.
        Field::MipsHalf16(Half::Ha) if r.rela_addend.is_none() => {
            let pair = pair?;
            let lo = read_field(ctx.holding(pair, 4)?, pair, 4, ctx.endian_le);
            a = (a << 16) + sext(lo & 0xffff, 16);
        }
        Field::Mips26 if r.rela_addend.is_none() && !target.is_some_and(|t| t.local) => {
            a = sext(a as u64, 28);
        }
        _ => {}
    }
    // `_gp_disp` is the distance to the GP of a PIC function, never modelled.
    if matches!(field, Field::MipsHalf16(_)) && target.is_some_and(|t| t.gp_disp) {
        return None;
    }

    let got_slot = |ctx: &mut Ctx<'_, '_>| -> Option<u64> {
        // A slot holds `S` alone; `got_slot` is `None` off an ET_REL.
        if value_kind_needs_zero(value_kind) && a != 0 {
            return None;
        }
        let t = target?;
        let (slot, width) = layout.got_slot(t.index)?;
        // A weak undefined or IFUNC slot's value is unknown: left unmapped.
        if !t.weak_undefined && !t.ifunc {
            ctx.got.insert(slot, (t.addr, width));
        }
        Some(slot)
    };
    let got_base = layout.got_base();
    let isa = target.map_or(Isa::Same, |t| t.isa);
    // A branch enters the instruction, not the symbol's ISA bit.
    let branch_s = match field {
        Field::ArmBranch24 { .. } | Field::ThumbBranch { .. } if isa == Isa::Thumb => s & !1,
        _ => s,
    };
    let sa = apply_addend(branch_s, a);
    let page = |x: u64| x & !0xfff;
    let value = match value_kind {
        Value::Abs | Value::Slot => sa,
        Value::Pcrel => sa.wrapping_sub(p),
        Value::PagePcrel => page(sa).wrapping_sub(page(p)),
        Value::GotPcrel => apply_addend(got_slot(ctx)?, a).wrapping_sub(p),
        Value::GotSlotPcrel => got_slot(ctx)?.wrapping_sub(p),
        Value::GotPagePcrel => page(got_slot(ctx)?).wrapping_sub(page(p)),
        Value::Got => got_slot(ctx)?,
        Value::I386Got if i386_got_absolute(holder?, p)? => apply_addend(got_slot(ctx)?, a),
        Value::GotOffset | Value::I386Got => {
            apply_addend(got_slot(ctx)?.wrapping_sub(got_base?), a)
        }
        Value::GotRelative => sa.wrapping_sub(got_base?),
        Value::GotBasePcrel => apply_addend(got_base?, a).wrapping_sub(p),
        Value::TocRelative => sa.wrapping_sub(layout.toc_base()?),
        Value::Toc => apply_addend(layout.toc_base()?, a),
        Value::ImageRelative => apply_addend(0, a),
        Value::Indirect | Value::Tls => return None,
        Value::PltCall => s.wrapping_sub(p),
        // A descriptor's entry is where `S + A` in `.opd` relocates to.
        Value::Ppc64Call { .. } if target?.in_opd => ctx.opd_entries.get(&sa)?.wrapping_sub(p),
        Value::Ppc64Call { notoc } => {
            let entry = if notoc {
                sa
            } else {
                sa.wrapping_add(target?.ppc64_local_entry)
            };
            entry.wrapping_sub(p)
        }
    };
    let word = match field {
        Field::Data { .. } => fit(obj, field, value)?,
        _ => encode(field, raw, value, isa, p)?,
    };
    Some(if thumb_le { swap_halfwords(word) } else { word })
}

/// Whether a GOT value only holds for a zero addend: the slot is `S`, so a
/// `Page(G)`, `G` or AArch64 `G - P` cannot carry one.
pub(super) fn value_kind_needs_zero(value: Value) -> bool {
    matches!(
        value,
        Value::GotPagePcrel | Value::Got | Value::GotSlotPcrel
    )
}

/// Whether the value reaches the symbol only through its GOT slot.
pub(super) fn value_kind_through_got(value: Value) -> bool {
    matches!(
        value,
        Value::GotPcrel
            | Value::GotSlotPcrel
            | Value::GotPagePcrel
            | Value::Got
            | Value::GotOffset
            | Value::I386Got
    )
}

/// Whether an i386 `GOT32` / `GOT32X` field at `p` is its slot's address
/// rather than its GOT offset: lld's test, a preceding ModRM with `mod = 0,
/// rm = 5`. `None` for a `lea` (`0x8d`), which GNU ld exempts. A field in its
/// region's first two bytes is an offset.
pub(super) fn i386_got_absolute(region: &MemRegion, p: u64) -> Option<bool> {
    let off = (p - region.start_addr()) as usize;
    if off < 2 || region.raw()[off - 1] & 0xc7 != 0x05 {
        return Some(false);
    }
    (region.raw()[off - 2] != 0x8d).then_some(true)
}

/// The file-initial `width`-byte field at `addr`, which `region` holds whole.
pub(super) fn read_field(region: &MemRegion, addr: u64, width: usize, endian_le: bool) -> u64 {
    let off = (addr - region.start_addr()) as usize;
    crate::bytes::read_uint(&region.raw()[off..off + width], endian_le)
}

/// A Thumb-2 instruction read as one little-endian word, with its halfwords
/// swapped to put the first one high, or back.
pub(super) fn swap_halfwords(word: u64) -> u64 {
    ((word & 0xffff) << 16) | ((word >> 16) & 0xffff)
}

/// A relocation's resolved symbol.
#[derive(Clone, Copy)]
pub(super) struct Target {
    /// `S`.
    addr: u64,
    /// Its symbol-table index, which picks its GOT slot.
    index: usize,
    /// The ISA an ARM function is entered in.
    isa: Isa,
    /// `STB_LOCAL`.
    local: bool,
    /// `STB_WEAK` and undefined: it may resolve to 0, so the synthetic extern
    /// address stands in only where it cannot be observed.
    weak_undefined: bool,
    /// A defined `STT_GNU_IFUNC`: `S` is an ET_REL's synthetic extern
    /// address, a linked image's resolver.
    ifunc: bool,
    /// MIPS `_gp_disp`.
    gp_disp: bool,
    /// Defined in `.opd` with no extern address, so `S` is a ppc64 ELFv1
    /// descriptor.
    in_opd: bool,
    /// How far past `S` a ppc64 ELFv2 local call enters; 0 at an extern
    /// address.
    ppc64_local_entry: u64,
}

/// Symbol `idx` from `.dynsym` (linked image) or `.symtab` (ET_REL), rebased
/// through `layout`; an ET_REL extern resolves to its `extern_address`.
/// `None` when it has no address: unresolved, no `.dynsym`, a bad
/// `st_shndx`, a linked image's zero-valued undefined or `SHN_COMMON` one.
pub(super) fn resolve_symbol_target(
    obj: &object::File<'_>,
    layout: &crate::elf::sections::ElfSectionLayout,
    idx: object::SymbolIndex,
) -> Option<Target> {
    let sym = reloc_symbol(obj, idx)?;
    let extern_addr = layout.extern_address(idx.0);
    let addr = match extern_addr {
        Some(addr) => addr,
        None => {
            // An undefined extern of a linked image: no address.
            if sym.address() == 0 && sym.is_undefined() {
                return None;
            }
            // `SHN_COMMON`'s `st_value` is its alignment, not an address.
            if sym.is_common() {
                return None;
            }
            // An `st_shndx` past the section table has no base.
            layout.try_symbol_address(&sym)?
        }
    };
    let arch = obj.architecture();
    let isa = match (arch, sym.kind()) {
        (A::Arm, object::SymbolKind::Text) if !sym.is_undefined() => {
            if sym.address() & 1 == 1 {
                Isa::Thumb
            } else {
                Isa::Arm
            }
        }
        _ => Isa::Same,
    };
    Some(Target {
        addr,
        index: idx.0,
        isa,
        local: sym.is_local(),
        weak_undefined: sym.is_weak() && sym.is_undefined(),
        ifunc: is_ifunc(&sym) && !sym.is_undefined(),
        gp_disp: matches!(arch, A::Mips | A::Mips64) && sym.name() == Ok("_gp_disp"),
        // An extern address is no descriptor. Arch test first: the name
        // lookup runs once per relocation.
        in_opd: arch == A::PowerPc64
            && extern_addr.is_none()
            && sym
                .section_index()
                .and_then(|i| obj.section_by_index(i).ok())
                .is_some_and(|sec| sec.name() == Ok(".opd")),
        ppc64_local_entry: if extern_addr.is_some() {
            0
        } else {
            local_entry_offset(obj, &sym)
        },
    })
}

/// How far past `sym` a call sharing its TOC enters: on ppc64 ELFv2 the
/// global-to-local entry distance, 0 elsewhere.
pub fn local_entry_offset<'data>(
    obj: &object::File<'data>,
    sym: &impl object::ObjectSymbol<'data>,
) -> u64 {
    match sym.flags() {
        object::SymbolFlags::Elf { st_other, .. }
            if obj.architecture() == A::PowerPc64 && !sym.is_undefined() =>
        {
            ppc64_local_entry_offset(st_other)
        }
        _ => 0,
    }
}

/// `PPC64_LOCAL_ENTRY_OFFSET`: bytes from a function's global entry to its
/// local one, from `st_other`'s top three bits.
pub(super) fn ppc64_local_entry_offset(st_other: u8) -> u64 {
    match st_other >> 5 {
        v @ 2..=6 => ((1u64 << v) >> 2) << 2,
        _ => 0,
    }
}

/// Where a MIPS relocation's field starts relative to `r_offset`:
/// `R_MIPS_16` is the low half of a 32-bit word, two bytes in on big-endian.
pub(super) fn mips_half_field_skew(r: &RelocInfo, arch: A, endian_le: bool) -> u64 {
    if !endian_le && matches!(arch, A::Mips | A::Mips64) && r.r_type == object::elf::R_MIPS_16 {
        2
    } else {
        0
    }
}
