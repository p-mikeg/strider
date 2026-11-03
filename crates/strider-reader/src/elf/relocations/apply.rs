//! Applying one relocation entry: resolving its symbol, computing its value
//! and encoding it into the field.

use object::{
    Architecture as A, Object, ObjectSection, ObjectSymbol, ObjectSymbolTable, RelocationKind,
};

use crate::MemRegion;
use crate::elf::encodings::{
    Field, Half, Isa, Kind, Value, classify, encode, implicit_addend, sext,
};

use super::implicit::note_copy;
use super::walk::RelocInfo;
use super::{Ctx, RelocSite};

/// Adds a possibly-negative relocation `addend` to a base address.
///
/// A negative addend casts to its 2's-complement bit pattern; `wrapping_add`
/// plus `Patch::new`'s truncation to the field width give the modular result
/// every relocation field expects.
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

/// `st_size` of symbol `idx`, from the table [`resolve_symbol_target`]
/// reads.
pub(super) fn symbol_size(obj: &object::File<'_>, idx: object::SymbolIndex) -> Option<u64> {
    let sym = if obj.kind() == object::ObjectKind::Relocatable {
        obj.symbol_by_index(idx)
    } else {
        obj.dynamic_symbol_table()?.symbol_by_index(idx)
    };
    Some(sym.ok()?.size())
}

/// A plain data field `object` describes: `Absolute` is `S + A`, `Relative`
/// and `PltRelative` `S + A - P`, the latter's L collapsing to S since no PLT
/// is materialised. Anything else is a hole.
pub(super) fn apply_plain(ctx: &mut Ctx<'_, '_>, site: RelocSite, r: &RelocInfo) {
    let obj = ctx.obj;
    let site_addr = site.addr;
    let endian_le = ctx.endian_le;

    // A mips64el implicit-addend entry, whose kind and size `object` may
    // have read off its symbol index.
    let Some(generic) = r.generic else {
        let holder = ctx.holder_or_hole(site, site_addr, UNKNOWN_FIELD_BYTES, r.type_word());
        ctx.unmodelled(holder, site_addr, UNKNOWN_FIELD_BYTES, r.type_word());
        return;
    };
    // `size` is in bits, and 0 nominally means "the kind's default". Absolute /
    // Relative / PltRelative all set it explicitly on every arch of interest,
    // so 0 here signals an arch-specific encoding that isn't modelled. The
    // width is needed before the value: an `SHT_REL` addend is read back out
    // of the field.
    let size_bits = generic.size_bits;
    let plain = size_bits != 0
        && size_bits.is_multiple_of(8)
        && size_bits <= 64
        // A byte-multiple size still does not make the field plain low bytes
        // at the offset: s390x's `*DBL` types hold `(S + A - P) >> 1`,
        // `R_LARCH_B16` a branch displacement, the SHARC `*_V3` family
        // instruction-encoded operands. `X86Signed` is the one non-`Generic`
        // encoding that IS plain: it only names the sign extension
        // `R_X86_64_32S` applies at runtime.
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
            UNKNOWN_FIELD_BYTES
        };
        let holder = ctx.holder_or_hole(site, site_addr, width, r.type_word());
        ctx.unmodelled(holder, site_addr, width, r.type_word());
        return;
    };
    let size_bytes = (size_bits / 8) as usize;

    // `r_offset` addresses the storage unit, inside which the field can be
    // offset. A `r_offset` at the very top of the address space is malformed;
    // the skew would carry the field out of it.
    let skew = mips_half_field_skew(r, obj.architecture(), endian_le);
    let Some(field_addr) = site_addr.checked_add(skew) else {
        return;
    };

    let holder = ctx.holder_or_hole(site, field_addr, size_bytes, r.type_word());
    // An `Absolute` target, an immediate with no symbol, has no `S` to add.
    let Some(target) = r
        .sym
        .and_then(|idx| resolve_symbol_target(obj, ctx.layout, idx))
    else {
        ctx.unmodelled(holder, field_addr, size_bytes, r.type_word());
        return;
    };
    // `PltRelative` is a call or jump, whose displacement hides the address it
    // was built from; every other plain field holds that address, see
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
    // `SHT_REL` keeps A in the field, sign-extended from its width: a
    // PC-relative site holds a negative A (-4 for an x86 `call rel32`). A
    // field no region holds contributes 0, its patch being skipped too.
    let addend = match r.rela_addend {
        Some(addend) => addend,
        None => holder
            .and_then(|region| {
                implicit_addend(field, read_field(region, field_addr, size_bytes, endian_le))
            })
            .unwrap_or(0),
    };
    // S, A, P follow the System V ABI generic relocation formula:
    // S = target_addr, A = addend, P = site_addr. P is the storage unit, not
    // the field inside it.
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

/// Field width assumed for a relocation whose type gives none: an instruction
/// word, or the 32-bit displacement of an x86 one.
pub(super) const UNKNOWN_FIELD_BYTES: usize = 4;

/// `value` checked against a plain data `field`. An ELF32 image's address
/// arithmetic wraps at 32 bits, so there a word always fits and a narrower
/// field is checked against the sign-extended 32-bit value.
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
    // An `SHT_REL` image-relative field already holds `B + A`, the image base
    // `B` modelled 0: nothing to patch, and nothing to hole either.
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
    // The common case by far: a RELA image-relative word reads no symbol or field.
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
    // Only a branch displacement and a GOT slot reference hide the address
    // they were built from, see `Target::weak_undefined`.
    if target.is_some_and(|t| t.weak_undefined)
        && !field.is_branch()
        && !value_kind_through_got(value_kind)
    {
        return None;
    }
    // An IFUNC's `S` is its resolver: a data word holding it is not known,
    // and only its GOT slot's address is. See `Ctx::in_code`.
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
        // A GOT exists only for an ET_REL, and a slot holds `S` alone.
        if value_kind_needs_zero(value_kind) && a != 0 {
            return None;
        }
        let t = target?;
        let (slot, width) = layout.got_slot(t.index)?;
        // A weak undefined symbol's slot holds 0 as readily as an address,
        // and an IFUNC's what its resolver returns, so either is left
        // unmapped and the load through it stays opaque.
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

/// Whether the value reaches the symbol only through its GOT slot, which is
/// then the one place its address appears.
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
/// rather than its offset from the GOT: lld's test, the byte before it read
/// as a ModRM with `mod = 0, rm = 5`, though it may be a SIB or a disp8.
/// `None` for a `lea` (`0x8d` before that byte), which GNU ld exempts. A
/// field in its section's first two bytes is an offset.
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
    /// `STB_WEAK` and undefined, so the link or the module loader resolves it
    /// to 0 as readily as to an address and `if (&w)` can go either way. The
    /// synthetic extern address stands in only where it cannot be observed,
    /// everything else being left uncomputed rather than folded one way.
    weak_undefined: bool,
    /// A defined `STT_GNU_IFUNC`, so `S` is its resolver and not the function
    /// the loader binds.
    ifunc: bool,
    /// MIPS `_gp_disp`.
    gp_disp: bool,
    /// Defined in `.opd`, so `S` is a ppc64 ELFv1 descriptor.
    in_opd: bool,
    /// How far past `S` a ppc64 ELFv2 local call enters.
    ppc64_local_entry: u64,
}

/// Symbol `idx`, resolved.
///
/// A linked image's index is into `.dynsym`, so `obj.symbol_by_index` alone
/// would return the wrong entry; ET_REL's per-section relocations reference
/// `.symtab`.
///
/// The address is rebased through `layout`: an ET_REL symbol's `st_value` is
/// an offset into its section, every section after the first at a given
/// `sh_addr` sits at a synthetic base, and an undefined or `SHN_COMMON` one
/// resolves to its [`ElfSectionLayout::extern_address`].
///
/// `None` when the index doesn't resolve (malformed ELF), when a linked image
/// has no `.dynsym`, when its `st_shndx` names no section header, when a
/// linked image's symbol is undefined with a zero value or `SHN_COMMON`.
///
/// [`ElfSectionLayout::extern_address`]: crate::elf::sections::ElfSectionLayout::extern_address
pub(super) fn resolve_symbol_target(
    obj: &object::File<'_>,
    layout: &crate::elf::sections::ElfSectionLayout,
    idx: object::SymbolIndex,
) -> Option<Target> {
    // An ET_REL's per-section `SHT_REL`/`SHT_RELA` indexes the `.symtab` its
    // `sh_link` names, never the dynamic table. `object`'s
    // `dynamic_symbol_table` is simply the first `SHT_DYNSYM` section in the
    // file, populated whatever the `e_type`, so an object file carrying one
    // otherwise sends every relocation to the wrong table and patches in an
    // unrelated symbol's `st_value`. A linked image's dynamic relocations
    // index `.dynsym`; with none, the index names a table the link discarded,
    // never `.symtab`.
    let sym = if obj.kind() == object::ObjectKind::Relocatable {
        obj.symbol_by_index(idx)
    } else {
        obj.dynamic_symbol_table()?.symbol_by_index(idx)
    }
    .ok()?;
    let addr = match layout.extern_address(idx.0) {
        Some(addr) => addr,
        None => {
            // `(0, undefined)` is a legitimate undefined or weak extern of a
            // linked image, tested on the raw `st_value`, which is what
            // "undefined" is expressed in.
            if sym.address() == 0 && sym.is_undefined() {
                return None;
            }
            // `SHN_COMMON` holds the symbol's alignment in `st_value`; its
            // address exists only once the link allocates it in `.bss`.
            if sym.is_common() {
                return None;
            }
            // An `st_shndx` past the section table: the offset it declares has
            // no base, so there is no address to patch.
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
        // ppc64 ELFv1 only, and the arch test comes first: the name lookup
        // below reads `.shstrtab` and this runs once per relocation, so on
        // every other arch it was a string compare per relocation for a field
        // nothing reads. `gp_disp` above short-circuits the same way.
        in_opd: arch == A::PowerPc64
            && sym
                .section_index()
                .and_then(|i| obj.section_by_index(i).ok())
                .is_some_and(|sec| sec.name() == Ok(".opd")),
        ppc64_local_entry: local_entry_offset(obj, &sym),
    })
}

/// How far past `sym` a call sharing its TOC enters: on ppc64 ELFv2 the
/// distance from a defined function's global entry to its local one, which
/// is where a `bl` sharing its TOC lands. 0 on every other arch.
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

/// Where a MIPS relocation's field starts relative to `r_offset`.
///
/// `R_MIPS_16`'s storage unit is the 32-bit word at `r_offset` and its field is
/// that word's low half, so on a big-endian target the two bytes to patch,
/// and the implicit addend to read back, start two bytes in. `R_MIPS_32` and
/// `R_MIPS_64`, the only other MIPS types `object` gives a width, fill their
/// storage unit exactly.
pub(super) fn mips_half_field_skew(r: &RelocInfo, arch: A, endian_le: bool) -> u64 {
    if !endian_le && matches!(arch, A::Mips | A::Mips64) && r.r_type == object::elf::R_MIPS_16 {
        2
    } else {
        0
    }
}

/// `STT_GNU_IFUNC`, from `st_info`'s low nibble.
pub(super) fn is_ifunc<'d>(sym: &impl ObjectSymbol<'d>) -> bool {
    matches!(
        sym.flags(),
        object::SymbolFlags::Elf { st_info, .. } if st_info & 0xf == object::elf::STT_GNU_IFUNC
    )
}
