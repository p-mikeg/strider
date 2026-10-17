//! ELF relocation application, as one sorted patch list per image that every
//! region shares and a read applies where it crosses a site.
//!
//! An image that has not been through its linker, or through `ld.so`, holds
//! only an addend, or nothing, in every cross-reference field. An ET_REL's
//! `call rel32` sits as `e8 fc ff ff ff` or `e8 00 00 00 00` under an
//! `R_X86_64_PLT32` the linker would resolve, and an AArch64 `bl` as a branch
//! to itself; an ET_DYN's dispatch-table slots and GOT/PLT entries hold what
//! `ld` left under `.rela.dyn` / `.rela.plt` until `ld.so` fills them. Every
//! one of those decodes as a plausible reference into the image itself. This
//! applies the relocations statically.
//!
//! A relocation whose value is not computed is not left in place: outside a
//! writable mapping its field becomes a hole no read serves, see
//! [`apply_elf_relocations_with`].

use object::{
    Architecture as A, Object, ObjectSection, ObjectSymbol, ObjectSymbolTable, RelocationFlags,
    RelocationKind, RelocationTarget,
};

use std::collections::BTreeMap;
use std::sync::Arc;

use super::encodings::{Field, Half, Isa, Kind, Value, classify, encode, implicit_addend, sext};
use super::sections::AddressSet;
use crate::{MemRegion, Patch, RegionIndex, Result};

/// Adds a possibly-negative relocation `addend` to a base address.
///
/// A negative addend casts to its 2's-complement bit pattern; `wrapping_add`
/// plus `Patch::new`'s truncation to the field width give the modular result
/// every relocation field expects.
#[inline]
fn apply_addend(base: u64, addend: i64) -> u64 {
    base.wrapping_add(addend as u64)
}

/// Attaches one relocation patch list, sorted by address, to every region,
/// walking the table appropriate for `obj.kind()`: `obj.dynamic_relocations()`
/// for ET_EXEC / ET_DYN, per-section `section.relocations()` for ET_REL. Each
/// region keeps the part that can overlap it, so a read searches no more
/// patches than a list of its own would hold.
///
/// # Supported
///
/// Every row [`classify`] lists, which says what each `(arch, r_type)`
/// computes and the field it is encoded into: instruction immediates, GOT and
/// TOC references, `R_*_RELATIVE` / `IRELATIVE` (image base modelled 0; an
/// `SHT_REL` one's field already holds the whole value and is served as
/// assembled), GOT/PLT slots and PC-relative words. A GOT reference resolves
/// through a synthetic GOT slot holding the symbol's address, which exists
/// only for an ET_REL.
///
/// Otherwise the plain data fields `object` describes: `Absolute` is `S + A`
/// (`R_X86_64_64`), `Relative` `S + A - P` (`R_X86_64_PC32`,
/// `R_AARCH64_PREL32`), and `PltRelative` valued the same, no PLT being
/// materialised (`R_X86_64_PLT32`).
///
/// `S` exists only for a symbol target: a value reading it against
/// STN_UNDEF is not computed.
///
/// On an ET_REL an undefined or `SHN_COMMON` symbol resolves to its
/// [`ElfSectionLayout::extern_address`], outside every mapping. An undefined
/// `STB_WEAK` one is the exception: see `Target::weak_undefined`.
///
/// An instruction field is read and written in the instruction byte order,
/// which on AArch64 is little-endian whatever `EI_DATA` says; a data field
/// follows `EI_DATA`.
///
/// # Not computed
///
/// A relocation whose value is not computed (a kind not listed above, a symbol
/// with no address or a weak undefined one, a value its field cannot hold, a
/// branch needing a veneer) leaves no plausible bytes behind: unless the field
/// is in a writable mapping, which nothing folds, it becomes a hole no read
/// serves, so decoding or folding through it fails.
///
/// Any patch list the regions already carry is replaced, so re-applying over
/// one region set is idempotent.
///
/// `layout` must be the one built for `obj`, `loaded_with` the filter the
/// regions were loaded with and `writable` the image's writable mappings.
///
/// Returns the GOT slots the relocations referenced, as regions of their own.
///
/// # Errors
///
/// When a loaded section's bytes cannot be read.
pub(crate) fn apply_elf_relocations_with(
    regions: &mut [MemRegion],
    obj: &object::File<'_>,
    loaded_with: super::sections::LoadFilter,
    layout: &super::sections::ElfSectionLayout,
    writable: &AddressSet,
    executable: &AddressSet,
) -> Result<Vec<MemRegion>> {
    let owners = super::sections::loaded_section_indices(obj, layout, loaded_with)?;
    // One lookup per relocation instead of a scan of every region; an ET_REL
    // carries one region per SHF_ALLOC section.
    let region_index = RegionIndex::new(regions);
    let mut ctx = Ctx {
        obj,
        layout,
        regions,
        region_index: &region_index,
        writable,
        executable,
        endian_le: matches!(obj.endianness(), object::Endianness::Little),
        insn_le: matches!(obj.endianness(), object::Endianness::Little)
            || obj.architecture() == A::Aarch64,
        opd_entries: super::sections::opd_entries(obj, layout),
        got: BTreeMap::new(),
        patches: Vec::new(),
    };
    for_each_reloc_site(obj, &owners, layout, |site_addr, avail, r, pair| {
        apply_one_relocation(
            &mut ctx,
            RelocSite {
                addr: site_addr,
                avail,
            },
            r,
            pair,
        );
    });
    let got = got_regions(&ctx.got, ctx.endian_le)?;
    let mut patches = ctx.patches;
    // Stable, so the last patch recorded at a site is the one a read applies.
    patches.sort_by_key(|p| p.addr);
    let patches: Arc<[Patch]> = patches.into();
    for region in regions {
        region.attach_patches(&patches);
    }
    Ok(got)
}

/// What every relocation in one walk reads, and what it records.
struct Ctx<'a, 'd> {
    obj: &'a object::File<'d>,
    layout: &'a super::sections::ElfSectionLayout,
    /// Unique starts (`collapse_equal_starts`), so the highest-start region
    /// holding a field is the one a read of it reaches.
    regions: &'a [MemRegion],
    region_index: &'a RegionIndex,
    writable: &'a AddressSet,
    executable: &'a AddressSet,
    endian_le: bool,
    /// Byte order of an instruction field: A64 instructions are little-endian
    /// whatever `EI_DATA` says, and only data follows it. ARM BE32 instructions
    /// really are big-endian, BE8's swap being the link's.
    insn_le: bool,
    /// ELFv1 descriptor address -> the code entry its first word relocates to.
    opd_entries: BTreeMap<u64, u64>,
    /// GOT slot address -> (the symbol address it holds, slot width).
    got: BTreeMap<u64, (u64, usize)>,
    /// Every patch the walk records, in record order.
    patches: Vec<Patch>,
}

impl<'a> Ctx<'a, '_> {
    /// The byte order `field` is read and patched in.
    fn field_le(&self, field: Field) -> bool {
        match field {
            Field::Data { .. } => self.endian_le,
            _ => self.insn_le,
        }
    }

    /// The highest-start region holding all of `[addr, addr + width)`: the
    /// one a read of the whole field is served from.
    fn holding(&self, addr: u64, width: usize) -> Option<&'a MemRegion> {
        let regions = self.regions;
        let i = self.region_index.covering(addr, width as u64).next()?;
        Some(&regions[i])
    }

    /// [`holding`](Self::holding) for a field of `site`, and `None` too when
    /// the field runs past `site.avail`, what is left of the section owning
    /// the site. A field past its section, or past the end of every mapping
    /// holding its first byte, would write a neighbour's bytes, so it is
    /// neither patched nor holed.
    fn holder(&self, site: RelocSite, addr: u64, width: usize) -> Option<&'a MemRegion> {
        // `addr` is `site.addr` plus a non-negative `mips_half_field_skew` and
        // `width` is at most 8, so neither wraps; saturating anyway keeps a
        // malformed pair on the `None` path.
        if addr.wrapping_sub(site.addr).saturating_add(width as u64) > site.avail {
            return None;
        }
        self.holding(addr, width)
    }

    /// Records the low `width` bytes of `value` at `addr` when a region holds
    /// the field. Every region overlapping it then serves the patch, clipped
    /// to its own bytes: a narrow read inside a field that straddles an inner
    /// region's end is that region's, a wide one the outer region's.
    fn record(
        &mut self,
        holder: Option<&MemRegion>,
        addr: u64,
        value: u64,
        width: usize,
        endian_le: bool,
    ) {
        if holder.is_some()
            && let Some(patch) = Patch::new(addr, value, width, endian_le)
        {
            self.patches.push(patch);
        }
    }

    /// A relocation whose value is not computed: a hole over the field, when
    /// a region holds it, unless the field is writable and NOT executable at
    /// runtime.
    ///
    /// Writable data the program rewrites anyway is not worth a hole.  A
    /// writable EXECUTABLE field is a different thing: `CodeAndReadOnly` keeps
    /// an RWX mapping on purpose (firmware, `ld -N`), so those bytes reach the
    /// decoder, and serving them unrelocated hands it a plausible wrong
    /// instruction with nothing said about it.
    fn unmodelled(&mut self, holder: Option<&MemRegion>, addr: u64, width: usize, r_type: u32) {
        if holder.is_some()
            && (!self.writable.touches(addr, width) || self.executable.touches(addr, width))
        {
            self.patches.push(Patch::hole(addr, width, r_type));
        }
    }
}

/// A relocation site: where the field lands, and how many bytes of the section
/// owning it remain from there.
#[derive(Clone, Copy)]
struct RelocSite {
    addr: u64,
    avail: u64,
}

/// One relocation, decoded once.
///
/// `object` reads a mips64el `Elf64_Rel` `r_info` as one little-endian `u64`,
/// which swaps `r_sym` with the type word (`r_ssym | r_type3 | r_type2 |
/// r_type`); that is undone here and nowhere else.
#[derive(Clone, Copy, Debug)]
pub(crate) struct RelocInfo {
    pub(crate) r_type: u32,
    /// MIPS64's composite second and third types; `R_MIPS_NONE` elsewhere.
    pub(crate) r_type2: u32,
    pub(crate) r_type3: u32,
    /// `None` is STN_UNDEF.
    pub(crate) sym: Option<object::SymbolIndex>,
    /// `r_addend`; `None` under `SHT_REL`, A then being in the field.
    pub(crate) rela_addend: Option<i64>,
    /// What `object` derived from the type; `None` for a mips64el
    /// implicit-addend entry, which it may have read off the wrong field.
    pub(crate) generic: Option<Generic>,
}

/// `object`'s own reading of a relocation type.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Generic {
    kind: RelocationKind,
    /// In bits; 0 for a type it does not size.
    size_bits: u8,
    encoding: object::RelocationEncoding,
}

impl RelocInfo {
    /// `crel`: the image holds an `SHT_CREL` table, whose entries `object`
    /// reads as written although it reports them implicit-addend too.
    fn decode(reloc: &object::Relocation, arch: A, endian_le: bool, crel: bool) -> Self {
        let RelocationFlags::Elf { r_type } = reloc.flags() else {
            unreachable!("an ELF relocation carries ELF flags");
        };
        let reported = match reloc.target() {
            RelocationTarget::Symbol(idx) => Some(idx),
            _ => None,
        };
        let rela_addend = (!reloc.has_implicit_addend()).then(|| reloc.addend());
        let mips64el_rel = arch == A::Mips64 && endian_le && rela_addend.is_none();
        let (sym, word) = if mips64el_rel && !crel {
            let word = reported.map_or(0, |idx| (idx.0 as u32).swap_bytes());
            let sym = (r_type != 0).then_some(object::SymbolIndex(r_type as usize));
            (sym, word)
        } else {
            (reported, r_type)
        };
        let (r_type, r_type2, r_type3) = if arch == A::Mips64 {
            (word & 0xff, (word >> 8) & 0xff, (word >> 16) & 0xff)
        } else {
            (word, object::elf::R_MIPS_NONE, object::elf::R_MIPS_NONE)
        };
        Self {
            r_type,
            r_type2,
            r_type3,
            sym,
            rela_addend,
            generic: (!mips64el_rel).then(|| Generic {
                kind: reloc.kind(),
                size_bits: reloc.size(),
                encoding: reloc.encoding(),
            }),
        }
    }

    /// `r_type3 << 16 | r_type2 << 8 | r_type`, the type a hole names.
    pub(crate) fn type_word(self) -> u32 {
        (self.r_type3 << 16) | (self.r_type2 << 8) | self.r_type
    }
}

/// Whether `obj` holds an `SHT_CREL` table. MIPS64 is always ELF64.
fn has_crel(obj: &object::File<'_>) -> bool {
    use object::read::elf::SectionHeader as _;
    match obj {
        object::File::Elf64(f) => f
            .elf_section_table()
            .iter()
            .any(|s| s.sh_type(f.endian()) == object::elf::SHT_CREL),
        _ => false,
    }
}

/// Invokes `f(site_addr, avail, reloc, pair)` per relocation site, `site_addr`
/// being the **absolute** virtual address in the coordinate system the loaded
/// regions live in and `avail` the bytes left of the section owning the site
/// (`u64::MAX` for a dynamic site, whose owner is a segment and which therefore
/// has no section end to overrun).
///
/// `pair` is, for an `SHT_REL` `R_MIPS_HI16`, the site of the `R_MIPS_LO16`
/// against the same symbol that follows it, whose field holds the low half of
/// the addend.
///
/// Kind dispatch:
///
/// * ET_REL: per-section tables, restricted to `owners`. A section's
///   `relocations()` yields `(r_offset, Relocation)` where `r_offset` is
///   relative to the section the relocations apply *to* (the one `sh_info`
///   points at), so the site is that section's `layout` base plus the offset.
/// * Everything else: the dynamic table, `owners` and `layout` unused.
///   `dynamic_relocations()` is `Some` for every ELF, but its iterator yields
///   only relocation sections whose `sh_link` is the dynamic symbol table's
///   index, which is 0 when there is no `.dynsym`. A static binary's
///   `.rela.plt` links to `.symtab`, so it walks no sites; `strip` rewrites
///   that link to 0, and the same binary then walks its IRELATIVE slots and
///   serves each resolver's address in place of the file bytes.
fn for_each_reloc_site<F>(
    obj: &object::File<'_>,
    owners: &std::collections::BTreeSet<usize>,
    layout: &super::sections::ElfSectionLayout,
    mut f: F,
) where
    F: FnMut(u64, u64, &RelocInfo, Option<u64>),
{
    let arch = obj.architecture();
    let endian_le = matches!(obj.endianness(), object::Endianness::Little);
    let crel = arch == A::Mips64 && has_crel(obj);
    match obj.kind() {
        object::ObjectKind::Relocatable => {
            for sec in obj.sections() {
                if !owners.contains(&sec.index().0) {
                    continue;
                }
                let sec_base = layout.section_base(&sec);
                let sec_size = sec.size();
                let relocs: Vec<(u64, RelocInfo)> = sec
                    .relocations()
                    .map(|(offset, reloc)| {
                        (offset, RelocInfo::decode(&reloc, arch, endian_le, crel))
                    })
                    .collect();
                let pairs = mips_hi16_pairs(&relocs, arch);
                for (i, (offset, r)) in relocs.iter().enumerate() {
                    // The gABI puts `r_offset` inside the section `sh_info`
                    // names.  A malformed object can point it past the end,
                    // where the site still lands in SOME loaded region and
                    // would silently patch an unrelated section's bytes.
                    // The reported size is 0 for every type dispatched on the
                    // raw `r_type`, so the budget travels to where the width is
                    // actually chosen instead of being checked here.
                    let avail = sec_size.saturating_sub(*offset);
                    let pair = pairs
                        .get(i)
                        .copied()
                        .flatten()
                        .map(|lo| sec_base.wrapping_add(lo));
                    f(sec_base.wrapping_add(*offset), avail, r, pair);
                }
            }
        }
        _ => {
            let Some(dyn_relocs) = obj.dynamic_relocations() else {
                return;
            };
            for (site_addr, reloc) in dyn_relocs {
                f(
                    site_addr,
                    u64::MAX,
                    &RelocInfo::decode(&reloc, arch, endian_le, crel),
                    None,
                );
            }
        }
    }
}

/// For each `SHT_REL` `R_MIPS_HI16` in `relocs`, the offset of the next
/// `R_MIPS_LO16` against the same symbol; empty unless there are any.
fn mips_hi16_pairs(relocs: &[(u64, RelocInfo)], arch: A) -> Vec<Option<u64>> {
    if !matches!(arch, A::Mips | A::Mips64) {
        return Vec::new();
    }
    let mut pairs = vec![None; relocs.len()];
    let mut next_lo: std::collections::HashMap<Option<object::SymbolIndex>, u64> =
        std::collections::HashMap::new();
    for (i, (offset, r)) in relocs.iter().enumerate().rev() {
        if r.rela_addend.is_some() {
            continue;
        }
        if r.r_type == object::elf::R_MIPS_LO16 {
            next_lo.insert(r.sym, *offset);
        } else if r.r_type == object::elf::R_MIPS_HI16 {
            pairs[i] = next_lo.get(&r.sym).copied();
        }
    }
    pairs
}

/// The referenced GOT slots, adjacent ones sharing a region.
fn got_regions(got: &BTreeMap<u64, (u64, usize)>, endian_le: bool) -> Result<Vec<MemRegion>> {
    let mut out = Vec::new();
    let mut run: Option<(u64, Vec<u8>)> = None;
    for (&addr, &(value, width)) in got {
        // `ElfSectionLayout::got_slot`, the only producer, yields the image's
        // word, 4 or 8, so this cannot fire -- but both slices below index
        // blind, and `8 - width` underflows for a wider one. Its twin
        // `Patch::new` already checks.
        if width == 0 || width > 8 {
            anyhow::bail!("GOT slot at {addr:#x} has width {width}, not 1..=8");
        }
        let bytes = if endian_le {
            value.to_le_bytes()[..width].to_vec()
        } else {
            value.to_be_bytes()[8 - width..].to_vec()
        };
        match &mut run {
            Some((start, data)) if *start + data.len() as u64 == addr => data.extend(bytes),
            _ => {
                if let Some((start, data)) = run.take() {
                    out.push(MemRegion::new(start, data)?);
                }
                run = Some((addr, bytes));
            }
        }
    }
    if let Some((start, data)) = run {
        out.push(MemRegion::new(start, data)?);
    }
    Ok(out)
}

/// Applies one relocation entry at the already-absolute `site.addr`: the row
/// [`classify`] names, else the plain field `object` describes, else a hole.
fn apply_one_relocation(ctx: &mut Ctx<'_, '_>, site: RelocSite, r: &RelocInfo, pair: Option<u64>) {
    match classify(ctx.obj.architecture(), r) {
        Some(Kind::Marker) => {}
        Some(Kind::Compute(value, field)) => apply_classified(ctx, site, r, pair, value, field),
        None => apply_plain(ctx, site, r),
    }
}

/// A plain data field `object` describes: `Absolute` is `S + A`, `Relative`
/// and `PltRelative` `S + A - P`, the latter's L collapsing to S since no PLT
/// is materialised. Anything else is a hole.
fn apply_plain(ctx: &mut Ctx<'_, '_>, site: RelocSite, r: &RelocInfo) {
    let obj = ctx.obj;
    let site_addr = site.addr;
    let endian_le = ctx.endian_le;

    // `object` read a mips64el `Elf64_Rel`'s kind and size off its symbol
    // index, so they describe nothing.
    let Some(generic) = r.generic else {
        let holder = ctx.holder(site, site_addr, UNKNOWN_FIELD_BYTES);
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
        let holder = ctx.holder(site, site_addr, width);
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

    let holder = ctx.holder(site, field_addr, size_bytes);
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
    if target.weak_undefined && !matches!(generic.kind, RelocationKind::PltRelative) {
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
    match fit(obj, field, value) {
        Some(value) => ctx.record(holder, field_addr, value, size_bytes, endian_le),
        None => ctx.unmodelled(holder, field_addr, size_bytes, r.type_word()),
    }
}

/// Field width assumed for a relocation whose type gives none: an instruction
/// word, or the 32-bit displacement of an x86 one.
const UNKNOWN_FIELD_BYTES: usize = 4;

/// `value` checked against a plain data `field`. An ELF32 image's address
/// arithmetic wraps at 32 bits, so there a word always fits and a narrower
/// field is checked against the sign-extended 32-bit value.
fn fit(obj: &object::File<'_>, field: Field, value: u64) -> Option<u64> {
    if obj.is_64() {
        return encode(field, 0, value, Isa::Same, 0);
    }
    let value = i64::from(value as u32 as i32) as u64;
    if field.width() >= 4 {
        return Some(value);
    }
    encode(field, 0, value, Isa::Same, 0)
}

/// A [`classify`]-listed relocation, see [`super::encodings`].
fn apply_classified(
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
    let holder = ctx.holder(site, p, width);
    // The common case by far: a RELA image-relative word reads no symbol or field.
    if let (Value::ImageRelative, Some(a)) = (value, r.rela_addend) {
        match fit(ctx.obj, field, apply_addend(0, a)) {
            Some(word) => ctx.record(holder, p, word, width, ctx.endian_le),
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
fn compute_classified(
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
        (true, None) => return None,
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
        if !layout.is_rebased() || (value_kind_needs_zero(value_kind) && a != 0) {
            return None;
        }
        let t = target?;
        let (slot, width) = layout.got_slot(t.index)?;
        // A weak undefined symbol's slot holds 0 as readily as an address, so
        // it is left unmapped and the load through it stays opaque.
        if !t.weak_undefined {
            ctx.got.insert(slot, (t.addr, width));
        }
        Some(slot)
    };
    let got_base = layout.is_rebased().then(|| layout.got_base());
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
        Value::GotPagePcrel => page(got_slot(ctx)?).wrapping_sub(page(p)),
        Value::Got => got_slot(ctx)?,
        Value::GotOffset => apply_addend(got_slot(ctx)?.wrapping_sub(got_base?), a),
        Value::GotRelative => sa.wrapping_sub(got_base?),
        Value::GotBasePcrel => apply_addend(got_base?, a).wrapping_sub(p),
        Value::TocRelative => sa.wrapping_sub(layout.toc_base()?),
        Value::Toc => apply_addend(layout.toc_base()?, a),
        Value::ImageRelative => apply_addend(0, a),
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
/// `Page(G)` or `G` cannot carry one.
fn value_kind_needs_zero(value: Value) -> bool {
    matches!(value, Value::GotPagePcrel | Value::Got)
}

/// Whether the value reaches the symbol only through its GOT slot, which is
/// then the one place its address appears.
fn value_kind_through_got(value: Value) -> bool {
    matches!(
        value,
        Value::GotPcrel | Value::GotPagePcrel | Value::Got | Value::GotOffset
    )
}

/// The file-initial `width`-byte field at `addr`, which `region` holds whole.
fn read_field(region: &MemRegion, addr: u64, width: usize, endian_le: bool) -> u64 {
    let off = (addr - region.start_addr()) as usize;
    let field = &region.raw()[off..off + width];
    let mut raw = 0u64;
    if endian_le {
        for (n, &b) in field.iter().enumerate() {
            raw |= u64::from(b) << (8 * n);
        }
    } else {
        for &b in field {
            raw = (raw << 8) | u64::from(b);
        }
    }
    raw
}

/// A Thumb-2 instruction read as one little-endian word, with its halfwords
/// swapped to put the first one high, or back.
fn swap_halfwords(word: u64) -> u64 {
    ((word & 0xffff) << 16) | ((word >> 16) & 0xffff)
}

/// A relocation's resolved symbol.
#[derive(Clone, Copy)]
struct Target {
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
    /// MIPS `_gp_disp`.
    gp_disp: bool,
    /// Defined in `.opd`, so `S` is a ppc64 ELFv1 descriptor.
    in_opd: bool,
    /// How far past `S` a ppc64 ELFv2 local call enters.
    ppc64_local_entry: u64,
}

/// Symbol `idx`, resolved.
///
/// Dispatches through `obj.dynamic_symbol_table()` first and falls back to the
/// static `.symtab` only when there is none. Indices in the dynamic table
/// reference `.dynsym`, so `obj.symbol_by_index` alone would return the wrong
/// entry; ET_REL's per-section relocations reference `.symtab`, which is what
/// the fallback resolves.
///
/// The address is rebased through `layout`: an ET_REL symbol's `st_value` is
/// an offset into its section, every section after the first at a given
/// `sh_addr` sits at a synthetic base, and an undefined or `SHN_COMMON` one
/// resolves to its [`ElfSectionLayout::extern_address`].
///
/// `None` when the index doesn't resolve (malformed ELF), when its `st_shndx`
/// names no section header, when a linked image's symbol is undefined with a
/// zero value or `SHN_COMMON`.
///
/// [`ElfSectionLayout::extern_address`]: super::sections::ElfSectionLayout::extern_address
fn resolve_symbol_target(
    obj: &object::File<'_>,
    layout: &super::sections::ElfSectionLayout,
    idx: object::SymbolIndex,
) -> Option<Target> {
    // An ET_REL's per-section `SHT_REL`/`SHT_RELA` indexes the `.symtab` its
    // `sh_link` names, never the dynamic table. `object`'s
    // `dynamic_symbol_table` is simply the first `SHT_DYNSYM` section in the
    // file, populated whatever the `e_type`, so an object file carrying one
    // otherwise sends every relocation to the wrong table and patches in an
    // unrelated symbol's `st_value`.
    let relocatable = obj.kind() == object::ObjectKind::Relocatable;
    let sym = obj
        .dynamic_symbol_table()
        .filter(|_| !relocatable)
        .map_or_else(|| obj.symbol_by_index(idx), |t| t.symbol_by_index(idx))
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
    let st_other = match sym.flags() {
        object::SymbolFlags::Elf { st_other, .. } => st_other,
        _ => 0,
    };
    Some(Target {
        addr,
        index: idx.0,
        isa,
        local: sym.is_local(),
        weak_undefined: sym.is_weak() && sym.is_undefined(),
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
        ppc64_local_entry: if arch == A::PowerPc64 && !sym.is_undefined() {
            ppc64_local_entry_offset(st_other)
        } else {
            0
        },
    })
}

/// `PPC64_LOCAL_ENTRY_OFFSET`: bytes from a function's global entry to its
/// local one, from `st_other`'s top three bits.
fn ppc64_local_entry_offset(st_other: u8) -> u64 {
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
fn mips_half_field_skew(r: &RelocInfo, arch: A, endian_le: bool) -> u64 {
    if !endian_le && matches!(arch, A::Mips | A::Mips64) && r.r_type == object::elf::R_MIPS_16 {
        2
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Re-applying over one region set overwrites the patch list rather than
    /// appending to it, so the bytes a second pass serves are the first pass's.
    /// Only reachable from inside the crate: a caller gets its relocations
    /// through [`crate::OwnedElf::regions`], which loads a fresh set each time.
    #[test]
    fn applying_twice_serves_what_applying_once_served() {
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/out/x64/elf_relocs.elf");
        assert!(
            path.exists(),
            "{} is not built; run `make -C fixtures`",
            path.display()
        );
        let filter = super::super::sections::LoadFilter::AllAllocatable;
        let owned = crate::OwnedElf::open(&path).expect("open");
        let obj = owned.checked_file().expect("the mapped file is unchanged");
        let layout = super::super::sections::ElfSectionLayout::new(&obj);
        let image = owned
            .regions_with(
                &obj,
                &layout,
                super::super::sections::RegionSource::Auto,
                filter,
                false,
            )
            .expect("regions");
        let (mut regions, writable, executable) = (image.regions, image.writable, image.executable);

        let bytes = |regions: &[MemRegion]| -> Vec<Vec<u8>> {
            regions
                .iter()
                .map(|r| {
                    let mut out = vec![0u8; (r.end_addr() - r.start_addr()) as usize];
                    r.read(r.start_addr(), &mut out);
                    out
                })
                .collect()
        };
        apply_elf_relocations_with(&mut regions, &obj, filter, &layout, &writable, &executable)
            .expect("apply");
        let once = bytes(&regions);
        apply_elf_relocations_with(&mut regions, &obj, filter, &layout, &writable, &executable)
            .expect("re-apply");
        assert_eq!(once, bytes(&regions));
    }

    /// Which region serves a read depends on the read's WIDTH, so a field
    /// straddling an inner region's end has to read back patched at every
    /// width: every region overlapping it keeps the patch.
    #[test]
    fn a_field_over_an_inner_regions_end_reads_back_patched_at_every_width() {
        let field = 0x1084;
        let value = 0xdead_beef_feed_face_u64;
        let mut regions = vec![
            MemRegion::new(0x1000, vec![0u8; 0x100]).unwrap(),
            MemRegion::new(0x1080, vec![0u8; 8]).unwrap(),
        ];
        let all: Arc<[Patch]> =
            vec![Patch::new(field, value, 8, true).expect("an 8 byte field")].into();
        for region in &mut regions {
            region.attach_patches(&all);
        }

        let table = crate::MemRegionsLookupTable::new(regions);
        let mut narrow = [0u8; 4];
        table
            .read_exact(field, &mut narrow)
            .expect("the inner region");
        let mut wide = [0u8; 8];
        table
            .read_exact(field, &mut wide)
            .expect("the outer region");
        assert_eq!(wide, value.to_le_bytes());
        assert_eq!(narrow, wide[..4]);
    }

    /// A field starting below a region's start still lands in it: a region
    /// keeps the patches up to `MAX_PATCH_LEN - 1` bytes before it, so the
    /// inner region serves the field's tail patched.
    #[test]
    fn a_field_over_an_inner_regions_start_reads_back_patched() {
        let field = 0x107c;
        let value = 0xdead_beef_feed_face_u64;
        let mut regions = vec![
            MemRegion::new(0x1000, vec![0u8; 0x100]).unwrap(),
            MemRegion::new(0x1080, vec![0u8; 8]).unwrap(),
        ];
        let all: Arc<[Patch]> =
            vec![Patch::new(field, value, 8, true).expect("an 8 byte field")].into();
        for region in &mut regions {
            region.attach_patches(&all);
        }

        let table = crate::MemRegionsLookupTable::new(regions);
        let mut tail = [0u8; 4];
        table
            .read_exact(0x1080, &mut tail)
            .expect("the inner region");
        assert_eq!(tail, value.to_le_bytes()[4..]);
    }

    /// A relocation the word domain builds: `r_type` against symbol 1 or
    /// STN_UNDEF, MIPS64's second type `r_type2`.
    fn word_info(r_type: u32, r_type2: u32, sym: bool) -> RelocInfo {
        RelocInfo {
            r_type,
            r_type2,
            r_type3: object::elf::R_MIPS_NONE,
            sym: sym.then_some(object::SymbolIndex(1)),
            rela_addend: Some(0),
            generic: None,
        }
    }

    /// Every type number below 1101 on every arch the word rows serve, against
    /// a symbol and none, MIPS64 with and without `R_MIPS_64` composed.
    fn word_domain() -> impl Iterator<Item = (A, RelocInfo)> {
        [
            A::X86_64,
            A::I386,
            A::Aarch64,
            A::Arm,
            A::PowerPc,
            A::PowerPc64,
            A::Mips,
            A::Mips64,
        ]
        .into_iter()
        .flat_map(|arch| {
            let seconds: &'static [u32] = if arch == A::Mips64 {
                &[object::elf::R_MIPS_NONE, object::elf::R_MIPS_64]
            } else {
                &[object::elf::R_MIPS_NONE]
            };
            seconds.iter().flat_map(move |&r_type2| {
                (0..=1100u32).flat_map(move |r_type| {
                    [false, true].map(move |sym| (arch, word_info(r_type, r_type2, sym)))
                })
            })
        })
    }

    const D4: Field = Field::Data {
        bytes: 4,
        signed: false,
    };
    const D8: Field = Field::Data {
        bytes: 8,
        signed: false,
    };
    const S4: Field = Field::Data {
        bytes: 4,
        signed: true,
    };
    const S8: Field = Field::Data {
        bytes: 8,
        signed: true,
    };

    /// `(arch, r_type, r_type2, symbol, value, field)`.
    type WordRow = (A, u32, u32, Option<bool>, Value, Field);

    /// The word rows, a `None` symbol matching both.
    const WORD_ROWS: &[WordRow] = {
        use Value::{Abs, ImageRelative as Ir, Pcrel, Slot};
        use object::elf as e;
        &[
            (A::X86_64, e::R_X86_64_RELATIVE, 0, None, Ir, D8),
            (A::X86_64, e::R_X86_64_IRELATIVE, 0, None, Ir, D8),
            (A::X86_64, e::R_X86_64_GLOB_DAT, 0, None, Slot, D8),
            (A::X86_64, e::R_X86_64_JUMP_SLOT, 0, None, Slot, D8),
            (A::I386, e::R_386_RELATIVE, 0, None, Ir, D4),
            (A::I386, e::R_386_IRELATIVE, 0, None, Ir, D4),
            (A::I386, e::R_386_GLOB_DAT, 0, None, Slot, D4),
            (A::I386, e::R_386_JMP_SLOT, 0, None, Slot, D4),
            (A::Aarch64, e::R_AARCH64_RELATIVE, 0, None, Ir, D8),
            (A::Aarch64, e::R_AARCH64_IRELATIVE, 0, None, Ir, D8),
            (A::Aarch64, e::R_AARCH64_GLOB_DAT, 0, None, Slot, D8),
            (A::Aarch64, e::R_AARCH64_JUMP_SLOT, 0, None, Slot, D8),
            (A::Arm, e::R_ARM_RELATIVE, 0, None, Ir, D4),
            (A::Arm, e::R_ARM_IRELATIVE, 0, None, Ir, D4),
            (A::Arm, e::R_ARM_GLOB_DAT, 0, None, Slot, D4),
            (A::Arm, e::R_ARM_JUMP_SLOT, 0, None, Slot, D4),
            (A::Arm, e::R_ARM_REL32, 0, None, Pcrel, S4),
            (A::PowerPc, e::R_PPC_RELATIVE, 0, None, Ir, D4),
            (A::PowerPc, e::R_PPC_IRELATIVE, 0, None, Ir, D4),
            (A::PowerPc, e::R_PPC_GLOB_DAT, 0, None, Slot, D4),
            (A::PowerPc, e::R_PPC_JMP_SLOT, 0, None, Slot, D4),
            (A::PowerPc, e::R_PPC_REL32, 0, None, Pcrel, S4),
            (A::PowerPc64, e::R_PPC64_RELATIVE, 0, None, Ir, D8),
            (A::PowerPc64, e::R_PPC64_IRELATIVE, 0, None, Ir, D8),
            (A::PowerPc64, e::R_PPC64_GLOB_DAT, 0, None, Slot, D8),
            (A::PowerPc64, e::R_PPC64_JMP_SLOT, 0, None, Slot, D8),
            (A::PowerPc64, e::R_PPC64_REL32, 0, None, Pcrel, S4),
            (A::PowerPc64, e::R_PPC64_REL64, 0, None, Pcrel, S8),
            (A::Mips, e::R_MIPS_REL32, 0, Some(false), Ir, D4),
            (A::Mips, e::R_MIPS_REL32, 0, Some(true), Abs, D4),
            (A::Mips, e::R_MIPS_GLOB_DAT, 0, None, Slot, D4),
            (A::Mips, e::R_MIPS_JUMP_SLOT, 0, None, Slot, D4),
            (A::Mips64, e::R_MIPS_REL32, 0, Some(false), Ir, D4),
            (
                A::Mips64,
                e::R_MIPS_REL32,
                e::R_MIPS_64,
                Some(false),
                Ir,
                D8,
            ),
            (A::Mips64, e::R_MIPS_REL32, 0, Some(true), Abs, D4),
            (
                A::Mips64,
                e::R_MIPS_REL32,
                e::R_MIPS_64,
                Some(true),
                Abs,
                D8,
            ),
            (A::Mips64, e::R_MIPS_GLOB_DAT, 0, None, Slot, D8),
            (A::Mips64, e::R_MIPS_GLOB_DAT, e::R_MIPS_64, None, Slot, D8),
            (A::Mips64, e::R_MIPS_JUMP_SLOT, 0, None, Slot, D8),
            (A::Mips64, e::R_MIPS_JUMP_SLOT, e::R_MIPS_64, None, Slot, D8),
        ]
    };

    /// The row `WORD_ROWS` gives `r` on `arch`.
    fn word_row(arch: A, r: &RelocInfo) -> Option<(Value, Field)> {
        WORD_ROWS
            .iter()
            .find(|row| {
                row.0 == arch
                    && row.1 == r.r_type
                    && row.2 == r.r_type2
                    && row.3.is_none_or(|sym| sym == r.sym.is_some())
            })
            .map(|row| (row.4, row.5))
    }

    /// The word rows `classify` answers, over every type number they could
    /// be; no other type takes an image-relative or slot row.
    #[test]
    fn classify_pins_the_word_rows() {
        let mut rows = 0;
        for (arch, r) in word_domain() {
            match word_row(arch, &r) {
                Some((value, field)) => {
                    rows += 1;
                    assert_eq!(
                        classify(arch, &r),
                        Some(Kind::Compute(value, field)),
                        "{arch:?} {r:?}"
                    );
                }
                None => assert!(
                    !matches!(
                        classify(arch, &r),
                        Some(Kind::Compute(Value::ImageRelative | Value::Slot, _))
                    ),
                    "{arch:?} {r:?} takes a word row WORD_ROWS lacks"
                ),
            }
        }
        assert_eq!(rows, 74, "40 rows, the 34 either-symbol ones counted twice");
    }
}
