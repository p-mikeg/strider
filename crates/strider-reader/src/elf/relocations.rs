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
//! A relocation whose value is not computed is not left in place where it
//! would be decoded or folded: its field becomes a hole no read serves, by the
//! rule [`Ctx::unmodelled`] states.

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
/// walking the tables the loader reads: for a linked image with a
/// `PT_DYNAMIC` the ones its tags place
/// ([`loader_view`](super::dynamic::loader_view)), else
/// `obj.dynamic_relocations()` for ET_EXEC / ET_DYN and per-section
/// `section.relocations()` for ET_REL. Each
/// region keeps the part that can overlap it, so a read searches no more
/// patches than a list of its own would hold.
///
/// # Supported
///
/// Every row [`classify`] lists, which says what each `(arch, r_type)`
/// computes and the field it is encoded into: instruction immediates, GOT and
/// TOC references, `R_*_RELATIVE` (image base modelled 0; an `SHT_REL` one's
/// field already holds the whole value and is served as assembled), GOT/PLT
/// slots and PC-relative words. A GOT reference resolves
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
/// [`ElfSectionLayout::extern_address`](super::sections::ElfSectionLayout::extern_address),
/// outside every mapping. An undefined `STB_WEAK` one is the exception: see
/// `Target::weak_undefined`.
///
/// An instruction field is read and written in the instruction byte order,
/// which on AArch64 is little-endian whatever `EI_DATA` says; a data field
/// follows `EI_DATA`.
///
/// # Not computed
///
/// A relocation whose value is not computed (a kind not listed above, a symbol
/// with no address or a weak undefined one, a value its field cannot hold, a
/// branch needing a veneer, an IFUNC's in data) leaves no plausible bytes
/// behind where they would be decoded or folded: [`Ctx::unmodelled`] says
/// which fields become holes, through which decoding or folding fails.
///
/// An IFUNC's value is what its resolver returns, which only running it
/// computes: `R_*_IRELATIVE`, and every value reading `S` of a defined
/// `STT_GNU_IFUNC` symbol, whose `st_value` is the resolver, outside an
/// executable mapping. A GOT reference to one still addresses its slot, which
/// is left unmapped; inside an executable mapping `S` is the symbol's address,
/// as [`Ctx::in_code`] says.
///
/// An `R_*_COPY` object holds what the loader copies from the library
/// defining it: every byte of its `st_size` is a hole, see [`hole_copies`].
///
/// A MIPS image's primary GOT holds entries no relocation record covers:
/// [`mips_implicit_got`] holes the ones the loader writes, and
/// [`lazy_got_header`] the ARM and PPC32 lazy-binding words.
///
/// Any patch list the regions already carry is replaced, so re-applying over
/// one region set is idempotent.
///
/// `layout` must be the one built for `obj`, `owners` the sections the regions
/// were loaded from ([`LoadedImage::owners`](super::sections::LoadedImage::owners))
/// and `writable` the image's writable mappings.
///
/// Returns the GOT slots the relocations referenced, as regions of their own.
///
/// # Errors
///
/// When the relocation tables
/// walked name more entries than [`RELOCATION_ENTRY_BUDGET`] times what the
/// file holds (see `without_repeated_tables` and
/// [`loader_view`](super::dynamic::loader_view)); and when a table the
/// dynamic tags place cannot be read or decoded and the loader may relocate
/// code (see `relocates_code`).
pub(crate) fn apply_elf_relocations_with(
    regions: &mut [MemRegion],
    obj: &object::File<'_>,
    owners: &std::collections::BTreeSet<usize>,
    layout: &super::sections::ElfSectionLayout,
    writable: &AddressSet,
    executable: &AddressSet,
    view: &super::dynamic::LoaderView,
) -> Result<Vec<MemRegion>> {
    debug_assert!(
        {
            let mut starts: Vec<u64> = regions.iter().map(MemRegion::start_addr).collect();
            starts.sort_unstable();
            starts.windows(2).all(|w| w[0] != w[1])
        },
        "two regions share a start"
    );
    match *view {
        super::dynamic::LoaderView::Unreadable(why) if relocates_code(obj) => anyhow::bail!(
            "the image relocates code, but {why}: what it relocates is unknown, \
             and its code would be served unrelocated. Load it with relocations \
             unapplied"
        ),
        super::dynamic::LoaderView::OverBudget { entries, budget } => anyhow::bail!(
            "the dynamic relocation tables name {entries} relocation entries, more \
             than the {budget} the file's length allows"
        ),
        _ => {}
    }
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
        copies: Vec::new(),
        patches: Vec::new(),
    };
    // A linked image with a `PT_DYNAMIC` is relocated from the tables its tags
    // place, as its own ELF; anything else from its section headers.
    let deduped;
    let (rewritten, dynamic) = match view {
        super::dynamic::LoaderView::NotDynamic => {
            deduped = without_repeated_tables(obj)?;
            (deduped.as_deref(), false)
        }
        super::dynamic::LoaderView::Tables { walked } => (walked.as_deref(), true),
        _ => (None, true),
    };
    let rewritten = rewritten.map(object::File::parse).transpose()?;
    let walked = match (&rewritten, dynamic) {
        (Some(walked), _) => Some(walked),
        (None, false) => Some(obj),
        (None, true) => None,
    };
    if let Some(walked) = walked {
        for_each_reloc_site(walked, owners, layout, |site_addr, avail, r, pair| {
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
        if !dynamic {
            relocations_off_dynsym(&mut ctx, walked);
        }
    }
    hole_copies(&mut ctx);
    mips_implicit_got(&mut ctx);
    lazy_got_header(&mut ctx);
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
    /// `[start, end)` of each `R_*_COPY` object, and its relocation type.
    copies: Vec<(u64, u64, u32)>,
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
    /// not patched. [`holder_or_hole`](Self::holder_or_hole) holes the
    /// latter, and the walk of tables off `.dynsym` asks for a hole's first
    /// byte only, which leaves no mapped byte of the field served.
    fn holder(&self, site: RelocSite, addr: u64, width: usize) -> Option<&'a MemRegion> {
        // `addr` is `site.addr` plus a non-negative `mips_half_field_skew` and
        // `width` is at most 8, so neither wraps; saturating anyway keeps a
        // malformed pair on the `None` path.
        if addr.wrapping_sub(site.addr).saturating_add(width as u64) > site.avail {
            return None;
        }
        self.holding(addr, width)
    }

    /// [`holder`](Self::holder), and a hole over the field when no one region
    /// holds it but one holds its first byte, inside `site.avail`: a field
    /// over two adjacent mappings, or past the end of the one holding its
    /// start, can be neither read nor patched whole.
    fn holder_or_hole(
        &mut self,
        site: RelocSite,
        addr: u64,
        width: usize,
        r_type: u32,
    ) -> Option<&'a MemRegion> {
        let holder = self.holder(site, addr, width);
        if holder.is_none()
            && addr.wrapping_sub(site.addr).saturating_add(width as u64) <= site.avail
        {
            let first = self.holding(addr, 1);
            self.unmodelled(first, addr, width, r_type);
        }
        holder
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

    /// Whether the field lies in an executable mapping, where it may be part
    /// of an instruction. There a relocation against a defined IFUNC takes
    /// the symbol's own address: a hole would stop the decoder, and the
    /// symbol is how the image names the function a `call` reaches (a
    /// FreeBSD kernel's `call memcpy`, which its linker binds at boot).
    ///
    /// A PC-relative call and a PC-relative `lea` look alike, so two limits
    /// follow: an address taken in code (`lea f(%rip)`, `mov $f`) and a data
    /// word in an executable mapping (FreeBSD 12's `.rodata`) take the
    /// symbol's address too, where at run time they hold the resolver's
    /// result.
    fn in_code(&self, addr: u64, width: usize) -> bool {
        self.executable.touches(addr, width)
    }

    /// A relocation whose value is not computed: a hole over the field, when
    /// a region holds it, unless the field is writable and NOT executable at
    /// runtime.
    ///
    /// Writable data the program rewrites anyway is not worth a hole.  A
    /// writable EXECUTABLE field is a different thing: `CodeAndReadOnly` keeps
    /// an RWX mapping on purpose (firmware, `ld -N`), so those bytes reach the
    /// decoder, and serving them unrelocated hands it a plausible wrong
    /// instruction with nothing said about it. A ROM view's served `.got` /
    /// `.data.rel.ro` has left `writable`, so a field there is a hole too.
    ///
    /// A field wider than a patch takes one hole per patch-wide piece.
    fn unmodelled(&mut self, holder: Option<&MemRegion>, addr: u64, width: usize, r_type: u32) {
        if holder.is_none() {
            return;
        }
        let end = addr.saturating_add(width as u64);
        let mut at = addr;
        while at < end {
            let piece = (end - at).min(crate::MAX_PATCH_LEN) as usize;
            if !self.writable.touches(at, piece) || self.executable.touches(at, piece) {
                self.patches.push(Patch::hole(at, piece, r_type));
            }
            at += piece as u64;
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
///   that link to 0, and the same binary then walks its IRELATIVE slots;
///   unstripped, [`relocations_off_dynsym`] walks them.
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
                    // `object` sizes only the types it knows, so `avail`
                    // travels to where the width is chosen instead of being
                    // checked here.
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

/// How many entries the relocation section headers a walk reads may name in
/// all, as a multiple of what the file holds at its smallest entry. Past it
/// the headers name the same bytes over and over, and the image is refused.
pub(crate) const RELOCATION_ENTRY_BUDGET: u64 = 2;

/// `obj`'s bytes with each relocation section header a walk reads that
/// repeats an earlier one's table, its `(sh_type, sh_offset, sh_size)` and on
/// an ET_REL its target, turned `SHT_NULL`; `None` when none repeats. A
/// repeat walks the same bytes, so dropping it changes nothing read.
///
/// A walk reads an ET_REL's header linked to `.symtab`, and a linked image's
/// linked to `.dynsym` or allocated. A linked image's repeats that disagree
/// on whether the entries index `.dynsym` leave the header kept allocated
/// and linked to none: every entry naming a symbol is a hole.
///
/// # Errors
///
/// When the headers name more entries than [`RELOCATION_ENTRY_BUDGET`] times
/// the file's length over its smallest entry (an `SHT_REL` one, or a byte
/// with an `SHT_CREL` table). A header whose table runs past the file names
/// none: no walk reads it.
fn without_repeated_tables(obj: &object::File<'_>) -> Result<Option<Vec<u8>>> {
    match obj {
        object::File::Elf32(f) => without_repeated_tables_of(f),
        object::File::Elf64(f) => without_repeated_tables_of(f),
        _ => Ok(None),
    }
}

fn without_repeated_tables_of<Elf: object::read::elf::FileHeader>(
    f: &object::read::elf::ElfFile<'_, Elf>,
) -> Result<Option<Vec<u8>>> {
    use object::read::elf::SectionHeader as _;
    let endian = f.endian();
    let relocatable = f.kind() == object::ObjectKind::Relocatable;
    let dynsym = f.elf_dynamic_symbol_table().section();
    let symtab = f.elf_symbol_table().section();
    let is_64 = f.is_64();
    let alloc = u64::from(object::elf::SHF_ALLOC);
    let file_len = f.data().len() as u64;
    let rel_size: u64 = if is_64 { 16 } else { 8 };
    // Table -> (the header kept, whether it resolves through `.dynsym`,
    // whether a repeat disagrees).
    let mut kept = std::collections::HashMap::new();
    let mut repeats = Vec::new();
    let (mut entries, mut smallest) = (0u64, rel_size);
    for (index, sh) in f.elf_section_table().iter().enumerate() {
        let ty = sh.sh_type(endian);
        let entry: u64 = match ty {
            object::elf::SHT_REL => rel_size,
            object::elf::SHT_RELA => rel_size / 2 * 3,
            object::elf::SHT_CREL => 1,
            _ => continue,
        };
        let link = sh.link(endian);
        let (target, resolves) = if relocatable {
            if link != symtab || sh.sh_info(endian) == 0 {
                continue;
            }
            (sh.sh_info(endian), true)
        } else if link == dynsym {
            (0, dynsym.0 != 0)
        } else if sh.sh_flags(endian).into() & alloc != 0 {
            (0, false)
        } else {
            continue;
        };
        let start: u64 = sh.sh_offset(endian).into();
        let size: u64 = sh.sh_size(endian).into();
        match kept.entry((ty, start, size, target)) {
            std::collections::hash_map::Entry::Vacant(v) => {
                v.insert((index, resolves, false));
                if start.checked_add(size).is_some_and(|end| end <= file_len) {
                    entries += size / entry;
                    smallest = smallest.min(entry);
                }
            }
            std::collections::hash_map::Entry::Occupied(mut o) => {
                let (_, first, mixed) = o.get_mut();
                *mixed |= *first != resolves;
                repeats.push(index);
            }
        }
    }
    let budget = RELOCATION_ENTRY_BUDGET * file_len / smallest;
    if entries > budget {
        anyhow::bail!(
            "relocation section headers name {entries} relocation entries, more than \
             {RELOCATION_ENTRY_BUDGET} times the {} a {file_len}-byte file holds",
            file_len / smallest
        );
    }
    if repeats.is_empty() {
        return Ok(None);
    }
    let header = f.elf_header();
    let shoff: u64 = header.e_shoff(endian).into();
    let shentsize = u64::from(header.e_shentsize(endian));
    let le = f.is_little_endian();
    let mut bytes = f.data().to_vec();
    let mut put = |index: usize, field: u64, value: u64, width: usize| -> Option<()> {
        let at = usize::try_from(shoff + index as u64 * shentsize + field).ok()?;
        crate::bytes::write_uint(bytes.get_mut(at..at + width)?, value, le);
        Some(())
    };
    // Elf32_Shdr / Elf64_Shdr: sh_type at 4, sh_flags at 8, sh_link at
    // 0x18 / 0x28.
    let (word, link_at) = if is_64 { (8, 0x28) } else { (4, 0x18) };
    let unlinked = kept.values().filter(|k| k.2).map(|k| k.0);
    let rewritten = || -> Option<()> {
        for index in repeats {
            put(index, 4, u64::from(object::elf::SHT_NULL), 4)?;
        }
        for index in unlinked {
            let sh = f
                .elf_section_table()
                .section(object::SectionIndex(index))
                .ok()?;
            put(index, 8, sh.sh_flags(endian).into() | alloc, word)?;
            put(index, link_at, 0, 4)?;
        }
        Some(())
    };
    // `object` reads the whole header table in-file at `sizeof(Shdr)` a
    // header, so every field written lies in `bytes`.
    rewritten().ok_or_else(|| anyhow::anyhow!("a section header lies outside the file"))?;
    Ok(Some(bytes))
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
        let mut bytes = vec![0u8; width];
        crate::bytes::write_uint(&mut bytes, value, endian_le);
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
        Some(Kind::Copy) => {
            let size = r.sym.and_then(|idx| symbol_size(ctx.obj, idx));
            note_copy(ctx, site.addr, size, r.type_word());
        }
        None => apply_plain(ctx, site, r),
    }
}

/// `st_size` of symbol `idx`, from the table [`resolve_symbol_target`]
/// reads.
fn symbol_size(obj: &object::File<'_>, idx: object::SymbolIndex) -> Option<u64> {
    let sym = if obj.kind() == object::ObjectKind::Relocatable {
        obj.symbol_by_index(idx)
    } else {
        obj.dynamic_symbol_table()?.symbol_by_index(idx)
    };
    Some(sym.ok()?.size())
}

/// Records the object an `R_*_COPY` at `addr` fills: `size` bytes, or with
/// no size known, up to the end of every region holding `addr`.
fn note_copy(ctx: &mut Ctx<'_, '_>, addr: u64, size: Option<u64>, r_type: u32) {
    let end = match size {
        Some(size) => addr.saturating_add(size),
        None => ctx
            .regions
            .iter()
            .filter(|region| region.contains(addr))
            .map(MemRegion::end_addr)
            .max()
            .unwrap_or(addr),
    };
    ctx.copies.push((addr, end, r_type));
}

/// Holes over every byte of every `R_*_COPY` object a region holds: what
/// the loader copies there is the defining library's, never the file's.
/// The objects are merged first, and each region finds the ones it overlaps
/// by a binary search, so the work is the regions' count times its log
/// plus the bytes held.
fn hole_copies(ctx: &mut Ctx<'_, '_>) {
    let copies = std::mem::take(&mut ctx.copies);
    let Some(&(_, _, r_type)) = copies.first() else {
        return;
    };
    let objects = AddressSet::merged(copies.iter().map(|&(lo, hi, _)| (lo, hi)).collect());
    let mut held = Vec::new();
    for region in ctx.regions {
        let (start, end) = (region.start_addr(), region.end_addr());
        for &(lo, hi) in objects.overlapping(start, end) {
            held.push((lo.max(start), hi.min(end)));
        }
    }
    for &(lo, hi) in AddressSet::merged(held).as_slice() {
        let holder = ctx.holding(lo, 1);
        ctx.unmodelled(holder, lo, (hi - lo) as usize, r_type);
    }
}

/// A plain data field `object` describes: `Absolute` is `S + A`, `Relative`
/// and `PltRelative` `S + A - P`, the latter's L collapsing to S since no PLT
/// is materialised. Anything else is a hole.
fn apply_plain(ctx: &mut Ctx<'_, '_>, site: RelocSite, r: &RelocInfo) {
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
fn value_kind_needs_zero(value: Value) -> bool {
    matches!(
        value,
        Value::GotPagePcrel | Value::Got | Value::GotSlotPcrel
    )
}

/// Whether the value reaches the symbol only through its GOT slot, which is
/// then the one place its address appears.
fn value_kind_through_got(value: Value) -> bool {
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
fn i386_got_absolute(region: &MemRegion, p: u64) -> Option<bool> {
    let off = (p - region.start_addr()) as usize;
    if off < 2 || region.raw()[off - 1] & 0xc7 != 0x05 {
        return Some(false);
    }
    (region.raw()[off - 2] != 0x8d).then_some(true)
}

/// The file-initial `width`-byte field at `addr`, which `region` holds whole.
fn read_field(region: &MemRegion, addr: u64, width: usize, endian_le: bool) -> u64 {
    let off = (addr - region.start_addr()) as usize;
    crate::bytes::read_uint(&region.raw()[off..off + width], endian_le)
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

/// A linked image's allocated relocation sections that `object`'s
/// `dynamic_relocations()` skips, their `sh_link` naming another table than
/// `.dynsym`, or none when there is a `.dynsym`: an unstripped static
/// executable's `.rela.plt`, linked to `.symtab`, or a table whose link was
/// left 0. Its `r_sym == 0` entries of a [`classify`] row, the IRELATIVE
/// slots among them, are applied as the dynamic table's are. Every other
/// entry is a hole over the field its row names, else over a word: one naming
/// a symbol because which table its index means is in doubt.
///
/// On MIPS every entry but `R_MIPS_NONE` and a MIPS32 `R_MIPS_COPY` is a
/// hole: its ELF64 type word is not decoded here.
///
/// `walked` is `ctx.obj`, or its copy with repeated tables dropped.
fn relocations_off_dynsym(ctx: &mut Ctx<'_, '_>, walked: &object::File<'_>) {
    let obj = ctx.obj;
    if matches!(obj.kind(), object::ObjectKind::Relocatable) {
        return;
    }
    let entries = match walked {
        object::File::Elf32(f) => entries_off_dynsym(f),
        object::File::Elf64(f) => entries_off_dynsym(f),
        _ => return,
    };
    let mips = matches!(obj.architecture(), A::Mips | A::Mips64);
    let word = if obj.is_64() { 8 } else { 4 };
    for (addr, r) in entries {
        let site = RelocSite {
            addr,
            avail: u64::MAX,
        };
        if mips && r.type_word() == object::elf::R_MIPS_NONE {
            continue;
        }
        // A MIPS32 type word is the type alone, so its COPY is known too.
        let kind = if !mips {
            classify(obj.architecture(), &r)
        } else if obj.architecture() == A::Mips && r.type_word() == object::elf::R_MIPS_COPY {
            Some(Kind::Copy)
        } else {
            None
        };
        let width = match kind {
            Some(Kind::Marker) => continue,
            // Which table the index means is in doubt, and with it the size.
            Some(Kind::Copy) => {
                note_copy(ctx, addr, None, r.type_word());
                continue;
            }
            Some(Kind::Compute(..)) if r.sym.is_none() => {
                apply_one_relocation(ctx, site, &r, None);
                continue;
            }
            Some(Kind::Compute(_, field)) => field.width(),
            None => word,
        };
        // Held from its first byte, the hole spans the whole field: each
        // region it overlaps serves its share, and none past them.
        let holder = ctx.holder(site, addr, 1);
        ctx.unmodelled(holder, addr, width, r.type_word());
    }
}

/// `(r_offset, entry)` of every `SHT_REL` / `SHT_RELA` / `SHT_CREL` entry in
/// an allocated section `object`'s `dynamic_relocations()` skips.
fn entries_off_dynsym<Elf: object::read::elf::FileHeader>(
    f: &object::read::elf::ElfFile<'_, Elf>,
) -> Vec<(u64, RelocInfo)> {
    use object::read::elf::SectionHeader as _;
    let endian = f.endian();
    let dynsym = f.elf_dynamic_symbol_table().section();
    f.elf_section_table()
        .iter()
        .filter(|sh| {
            sh.sh_flags(endian).into() & u64::from(object::elf::SHF_ALLOC) != 0
                && sh.link(endian) != dynsym
        })
        .flat_map(|sh| raw_entries(f, sh))
        .map(|(offset, r_type, sym, rela_addend)| {
            let r = RelocInfo {
                r_type,
                r_type2: object::elf::R_MIPS_NONE,
                r_type3: object::elf::R_MIPS_NONE,
                sym: (sym != 0).then_some(object::SymbolIndex(sym as usize)),
                rela_addend,
                generic: None,
            };
            (offset, r)
        })
        .collect()
}

/// `(r_offset, r_type, r_sym, r_addend)` of each entry of the relocation
/// section `sh`, as it lies: `r_addend` is `None` in an implicit-addend
/// table. Empty when `sh` holds no table that decodes; a CREL stream stops
/// at its first bad entry.
pub(crate) fn raw_entries<Elf: object::read::elf::FileHeader>(
    f: &object::read::elf::ElfFile<'_, Elf>,
    sh: &Elf::SectionHeader,
) -> Vec<(u64, u32, u32, Option<i64>)> {
    use object::read::elf::{Rel as _, Rela as _, SectionHeader as _};
    let endian = f.endian();
    let data = f.data();
    let mips64el = f.elf_header().is_mips64el(endian);
    if let Ok(Some((rela, _))) = sh.rela(endian, data) {
        rela.iter()
            .map(|e| {
                (
                    e.r_offset(endian).into(),
                    e.r_type(endian, mips64el),
                    e.r_sym(endian, mips64el),
                    Some(e.r_addend(endian).into()),
                )
            })
            .collect()
    } else if let Ok(Some((rel, _))) = sh.rel(endian, data) {
        rel.iter()
            .map(|e| {
                (
                    e.r_offset(endian).into(),
                    e.r_type(endian),
                    e.r_sym(endian),
                    None,
                )
            })
            .collect()
    } else if sh.sh_type(endian) == object::elf::SHT_CREL
        && let Ok(bytes) = sh.data(endian, data)
        && let Ok(crel) = object::read::elf::CrelIterator::new(bytes)
    {
        let rela = crel.is_rela();
        crel.map_while(|e| e.ok())
            .map(|e| (e.r_offset, e.r_type, e.r_sym, rela.then_some(e.r_addend)))
            .collect()
    } else {
        Vec::new()
    }
}

/// `STT_GNU_IFUNC`, from `st_info`'s low nibble.
fn is_ifunc<'d>(sym: &impl ObjectSymbol<'d>) -> bool {
    matches!(
        sym.flags(),
        object::SymbolFlags::Elf { st_info, .. } if st_info & 0xf == object::elf::STT_GNU_IFUNC
    )
}

/// Holes over the MIPS primary GOT entries the dynamic loader writes, which
/// no relocation record covers (MIPS psABI, "Global Offset Table"; glibc's
/// `elf_machine_runtime_setup` and `elf_machine_got_rel`):
///
/// * the lazy resolver's word, and the module pointer's when the top bit of
///   the second word asks for one;
/// * every global entry, `DT_MIPS_GOTSYM .. DT_MIPS_SYMTABNO`, except one
///   whose `.dynsym` symbol is defined in a section or absolute, is no IFUNC,
///   and is already held at its `st_value`. That is the address the loader's
///   lookup finds for a symbol the image defines, as a `GLOB_DAT` is taken to
///   resolve. An imported symbol's entry holds 0, or its lazy-binding stub,
///   which the loader replaces before the program runs or on the first call.
///
/// The local entries hold `B + value`, `B` modelled 0, and stay as the file
/// has them. `object` builds its dynamic symbol table from an `SHT_DYNSYM`
/// SECTION HEADER alone, so a section-stripped image has none and no symbol
/// can be checked: every global entry is a hole, as it is for a dynamic image
/// missing one of the tags that place the GOT, which makes every `.got` word
/// one. A hole names `R_MIPS_GLOB_DAT`, the relocation that covers the same
/// kind of entry outside the primary GOT.
fn mips_implicit_got(ctx: &mut Ctx<'_, '_>) {
    let obj = ctx.obj;
    if !matches!(obj.architecture(), A::Mips | A::Mips64)
        || !matches!(
            obj.kind(),
            object::ObjectKind::Executable | object::ObjectKind::Dynamic
        )
    {
        return;
    }
    let word: u64 = if obj.is_64() { 8 } else { 4 };
    let width = word as usize;
    let tags = match mips_got_tags(obj) {
        None => return,
        Some(Some(tags)) => tags,
        // The loader reads every one of the tags, so an image missing one says
        // nothing about which words it writes: every `.got` word is a hole.
        Some(None) => {
            for sec in obj.sections().filter(|s| s.name() == Ok(".got")) {
                let base = ctx.layout.section_base(&sec);
                // Up to the end of the mapping holding it, whatever `sh_size`
                // claims.
                for i in 0..sec.size() / word {
                    let Some(addr) = i.checked_mul(word).and_then(|o| base.checked_add(o)) else {
                        break;
                    };
                    let Some(holder) = ctx.holding(addr, width) else {
                        break;
                    };
                    ctx.unmodelled(Some(holder), addr, width, object::elf::R_MIPS_GLOB_DAT);
                }
            }
            return;
        }
    };
    let slot = |i: u64| {
        i.checked_mul(word)
            .and_then(|off| tags.base.checked_add(off))
    };
    let hole = |ctx: &mut Ctx<'_, '_>, addr: u64| {
        let holder = ctx.holding(addr, width);
        ctx.unmodelled(holder, addr, width, object::elf::R_MIPS_GLOB_DAT);
    };

    if tags.local_gotno >= 1
        && let Some(addr) = slot(0)
    {
        hole(ctx, addr);
    }
    if tags.local_gotno >= 2
        && let Some(addr) = slot(1)
        && let Some(region) = ctx.holding(addr, width)
        && read_field(region, addr, width, ctx.endian_le) >> (word * 8 - 1) != 0
    {
        hole(ctx, addr);
    }

    // `.dynsym` says which entries the loader leaves as they are.
    let dynsym = obj.dynamic_symbol_table();
    let globals = tags.symtabno.saturating_sub(tags.gotsym);
    for i in 0..globals {
        let Some(addr) = tags.local_gotno.checked_add(i).and_then(slot) else {
            break;
        };
        // Past the mapping holding the GOT nothing is served, so a count
        // running past it stops here.
        let Some(region) = ctx.holding(addr, width) else {
            break;
        };
        let held = read_field(region, addr, width, ctx.endian_le);
        let index = object::SymbolIndex((tags.gotsym + i) as usize);
        let served = dynsym
            .as_ref()
            .and_then(|table| table.symbol_by_index(index).ok())
            .is_some_and(|sym| {
                matches!(
                    sym.section(),
                    object::SymbolSection::Section(_) | object::SymbolSection::Absolute
                ) && !is_ifunc(&sym)
                    && held == sym.address()
            });
        if !served {
            hole(ctx, addr);
        }
    }
}

/// Holes over the two GOT header words the dynamic loader writes for lazy
/// binding, which no relocation record covers: ARM's link map and resolver
/// at `DT_PLTGOT + 4` / `+ 8` (glibc's `elf_machine_runtime_setup`), which
/// PLT0 jumps through, and the secure-PLT PPC32 pair at `DT_PPC_GOT + 4` /
/// `+ 8`. Only an image with a `DT_JMPREL` has them; `DF_BIND_NOW` or not,
/// a hole costing only the fold. A hole names the arch's `JUMP_SLOT`.
fn lazy_got_header(ctx: &mut Ctx<'_, '_>) {
    use object::elf;
    let obj = ctx.obj;
    let (base_tag, r_type) = match obj.architecture() {
        A::Arm => (elf::DT_PLTGOT, elf::R_ARM_JUMP_SLOT),
        A::PowerPc => (elf::DT_PPC_GOT, elf::R_PPC_JMP_SLOT),
        _ => return,
    };
    let Some(Some(tags)) = super::dynamic::dynamic_tags(obj) else {
        return;
    };
    let (Some(base), Some(_)) = (tags.get(base_tag), tags.get(elf::DT_JMPREL)) else {
        return;
    };
    for off in [4, 8] {
        let Some(addr) = base.checked_add(off) else {
            return;
        };
        let holder = ctx.holding(addr, 4);
        ctx.unmodelled(holder, addr, 4, r_type);
    }
}

/// The dynamic tags that place a MIPS primary GOT.
struct MipsGotTags {
    /// `DT_PLTGOT`, the GOT's start.
    base: u64,
    /// `DT_MIPS_LOCAL_GOTNO`, the reserved and local entries before the
    /// global ones.
    local_gotno: u64,
    /// `DT_MIPS_GOTSYM`, the `.dynsym` index of the first global entry's
    /// symbol; `DT_MIPS_SYMTABNO` with no global entries.
    gotsym: u64,
    /// `DT_MIPS_SYMTABNO`, the `.dynsym` count.
    symtabno: u64,
}

/// [`MipsGotTags`] from the loader's dynamic array: `None` without a
/// `PT_DYNAMIC`, `Some(None)` when the array cannot be read or lacks any of
/// the four tags.
fn mips_got_tags(obj: &object::File<'_>) -> Option<Option<MipsGotTags>> {
    let Some(tags) = super::dynamic::dynamic_tags(obj)? else {
        return Some(None);
    };
    let tag = |want: u32| tags.get(want);
    let (Some(base), Some(local_gotno), Some(gotsym), Some(symtabno)) = (
        tag(object::elf::DT_PLTGOT),
        tag(object::elf::DT_MIPS_LOCAL_GOTNO),
        tag(object::elf::DT_MIPS_GOTSYM),
        tag(object::elf::DT_MIPS_SYMTABNO),
    ) else {
        return Some(None);
    };
    Some(Some(MipsGotTags {
        base,
        local_gotno,
        gotsym,
        symtabno,
    }))
}

/// Whether the loader may relocate code: `DT_TEXTREL`, `DF_TEXTREL`, or a
/// `PT_LOAD` both writable and executable. A dynamic array that cannot be
/// read says nothing, so it may.
fn relocates_code(obj: &object::File<'_>) -> bool {
    fn rwx<Elf: object::read::elf::FileHeader>(f: &object::read::elf::ElfFile<'_, Elf>) -> bool {
        use object::read::elf::ProgramHeader as _;
        let endian = f.endian();
        let rwx = object::elf::PF_W | object::elf::PF_X;
        f.elf_program_headers()
            .iter()
            .any(|ph| ph.p_type(endian) == object::elf::PT_LOAD && ph.p_flags(endian) & rwx == rwx)
    }
    let textrel = match super::dynamic::dynamic_tags(obj) {
        None => false,
        Some(None) => true,
        Some(Some(t)) => {
            t.get(object::elf::DT_TEXTREL).is_some()
                || t.get(object::elf::DT_FLAGS)
                    .is_some_and(|v| v & u64::from(object::elf::DF_TEXTREL) != 0)
        }
    };
    textrel
        || match obj {
            object::File::Elf32(f) => rwx(f),
            object::File::Elf64(f) => rwx(f),
            _ => false,
        }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Re-applying over one region set overwrites the patch list rather than
    /// appending to it, so the bytes a second pass serves are the first pass's.
    /// Only reachable from inside the crate: a caller gets its relocations
    /// through [`crate::OwnedElf::image`], which loads a fresh set each time.
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
                &super::super::dynamic::LoaderView::NotDynamic,
            )
            .expect("regions");
        let (mut regions, writable, executable, owners) = (
            image.regions,
            image.writable,
            image.executable,
            image.owners,
        );

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
        apply_elf_relocations_with(
            &mut regions,
            &obj,
            &owners,
            &layout,
            &writable,
            &executable,
            &super::super::dynamic::loader_view(&obj),
        )
        .expect("apply");
        let once = bytes(&regions);
        apply_elf_relocations_with(
            &mut regions,
            &obj,
            &owners,
            &layout,
            &writable,
            &executable,
            &super::super::dynamic::loader_view(&obj),
        )
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

    /// A field starting at a region's last byte is that region's too: a
    /// one-byte read there serves the field's first byte patched.
    #[test]
    fn a_field_at_an_inner_regions_last_byte_reads_back_patched() {
        let field = 0x1087;
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
        let mut last = [0u8; 1];
        table
            .read_exact(field, &mut last)
            .expect("the inner region");
        assert_eq!(last, value.to_le_bytes()[..1]);
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
    /// STN_UNDEF, MIPS64's second and third types `r_type2` / `r_type3`.
    fn word_info(r_type: u32, (r_type2, r_type3): (u32, u32), sym: bool) -> RelocInfo {
        RelocInfo {
            r_type,
            r_type2,
            r_type3,
            sym: sym.then_some(object::SymbolIndex(1)),
            rela_addend: Some(0),
            generic: None,
        }
    }

    /// Every type number below 1101 on every arch the word rows serve, against
    /// a symbol and none, MIPS64 with no composite, with `R_MIPS_64` or
    /// `R_MIPS_SUB` second, and with `R_MIPS_64` then `R_MIPS_SUB`.
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
            use object::elf::{R_MIPS_64, R_MIPS_NONE, R_MIPS_SUB};
            let composites: &'static [(u32, u32)] = if arch == A::Mips64 {
                &[
                    (R_MIPS_NONE, R_MIPS_NONE),
                    (R_MIPS_64, R_MIPS_NONE),
                    (R_MIPS_SUB, R_MIPS_NONE),
                    (R_MIPS_64, R_MIPS_SUB),
                ]
            } else {
                &[(R_MIPS_NONE, R_MIPS_NONE)]
            };
            composites.iter().flat_map(move |&rest| {
                (0..=1100u32).flat_map(move |r_type| {
                    [false, true].map(move |sym| (arch, word_info(r_type, rest, sym)))
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

    /// `(arch, r_type, r_type2, symbol, value, field)`; a third type changes
    /// no row.
    type WordRow = (A, u32, u32, Option<bool>, Value, Field);

    /// The word rows, a `None` symbol matching both.
    const WORD_ROWS: &[WordRow] = {
        use Value::{Abs, ImageRelative as Ir, Indirect as Ind, Pcrel, Slot};
        use object::elf as e;
        &[
            (A::X86_64, e::R_X86_64_RELATIVE, 0, None, Ir, D8),
            (A::X86_64, e::R_X86_64_IRELATIVE, 0, None, Ind, D8),
            (A::X86_64, e::R_X86_64_GLOB_DAT, 0, None, Slot, D8),
            (A::X86_64, e::R_X86_64_JUMP_SLOT, 0, None, Slot, D8),
            (A::I386, e::R_386_RELATIVE, 0, None, Ir, D4),
            (A::I386, e::R_386_IRELATIVE, 0, None, Ind, D4),
            (A::I386, e::R_386_GLOB_DAT, 0, None, Slot, D4),
            (A::I386, e::R_386_JMP_SLOT, 0, None, Slot, D4),
            (A::Aarch64, e::R_AARCH64_RELATIVE, 0, None, Ir, D8),
            (A::Aarch64, e::R_AARCH64_IRELATIVE, 0, None, Ind, D8),
            (A::Aarch64, e::R_AARCH64_GLOB_DAT, 0, None, Slot, D8),
            (A::Aarch64, e::R_AARCH64_JUMP_SLOT, 0, None, Slot, D8),
            (A::Arm, e::R_ARM_RELATIVE, 0, None, Ir, D4),
            (A::Arm, e::R_ARM_IRELATIVE, 0, None, Ind, D4),
            (A::Arm, e::R_ARM_GLOB_DAT, 0, None, Slot, D4),
            (A::Arm, e::R_ARM_JUMP_SLOT, 0, None, Slot, D4),
            (A::Arm, e::R_ARM_REL32, 0, None, Pcrel, S4),
            (A::PowerPc, e::R_PPC_RELATIVE, 0, None, Ir, D4),
            (A::PowerPc, e::R_PPC_IRELATIVE, 0, None, Ind, D4),
            (A::PowerPc, e::R_PPC_GLOB_DAT, 0, None, Slot, D4),
            (A::PowerPc, e::R_PPC_JMP_SLOT, 0, None, Slot, D4),
            (A::PowerPc, e::R_PPC_REL32, 0, None, Pcrel, S4),
            (A::PowerPc64, e::R_PPC64_RELATIVE, 0, None, Ir, D8),
            (A::PowerPc64, e::R_PPC64_IRELATIVE, 0, None, Ind, D8),
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
            (
                A::Mips64,
                e::R_MIPS_REL32,
                e::R_MIPS_SUB,
                Some(false),
                Ir,
                D4,
            ),
            (
                A::Mips64,
                e::R_MIPS_REL32,
                e::R_MIPS_SUB,
                Some(true),
                Abs,
                D4,
            ),
            (A::Mips64, e::R_MIPS_GLOB_DAT, 0, None, Slot, D8),
            (A::Mips64, e::R_MIPS_GLOB_DAT, e::R_MIPS_64, None, Slot, D8),
            (A::Mips64, e::R_MIPS_GLOB_DAT, e::R_MIPS_SUB, None, Slot, D8),
            (A::Mips64, e::R_MIPS_JUMP_SLOT, 0, None, Slot, D8),
            (A::Mips64, e::R_MIPS_JUMP_SLOT, e::R_MIPS_64, None, Slot, D8),
            (
                A::Mips64,
                e::R_MIPS_JUMP_SLOT,
                e::R_MIPS_SUB,
                None,
                Slot,
                D8,
            ),
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
    /// be; no other type takes an image-relative, indirect or slot row.
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
                        Some(Kind::Compute(
                            Value::ImageRelative | Value::Indirect | Value::Slot,
                            _
                        ))
                    ),
                    "{arch:?} {r:?} takes a word row WORD_ROWS lacks"
                ),
            }
        }
        assert_eq!(
            rows, 86,
            "44 rows, the 36 either-symbol ones counted twice, and the 4 \
             `R_MIPS_64` ones 6 more times under a third type"
        );
    }

    /// `R_*_COPY` is the one copy row on each arch, and only as a single
    /// MIPS type.
    #[test]
    fn classify_pins_the_copy_rows() {
        use object::elf as e;
        let copy = |arch: A| match arch {
            A::X86_64 => e::R_X86_64_COPY,
            A::I386 => e::R_386_COPY,
            A::Aarch64 => e::R_AARCH64_COPY,
            A::Arm => e::R_ARM_COPY,
            A::PowerPc | A::PowerPc64 => e::R_PPC_COPY,
            _ => e::R_MIPS_COPY,
        };
        let mut rows = 0;
        for (arch, r) in word_domain() {
            let want = r.r_type == copy(arch) && r.r_type2 == e::R_MIPS_NONE;
            rows += usize::from(want);
            assert_eq!(
                classify(arch, &r) == Some(Kind::Copy),
                want,
                "{arch:?} {r:?}"
            );
        }
        assert_eq!(rows, 16, "8 arches, against a symbol and none");
    }

    /// The TLS rows and their widths; PPC32 shares the numbers of its 64-bit
    /// twins, whose 4-byte fields take no row, and the MIPS rows are
    /// MIPS64's alone.
    #[test]
    fn classify_pins_the_tls_rows() {
        use object::elf as e;
        let rows: &[(A, u32, u8)] = &[
            (A::X86_64, e::R_X86_64_DTPMOD64, 8),
            (A::X86_64, e::R_X86_64_DTPOFF64, 8),
            (A::X86_64, e::R_X86_64_TPOFF64, 8),
            (A::X86_64, e::R_X86_64_TLSDESC, 16),
            (A::I386, e::R_386_TLS_DESC, 8),
            (A::Aarch64, e::R_AARCH64_TLS_DTPMOD, 8),
            (A::Aarch64, e::R_AARCH64_TLS_DTPREL, 8),
            (A::Aarch64, e::R_AARCH64_TLS_TPREL, 8),
            (A::Aarch64, e::R_AARCH64_TLSDESC, 16),
            (A::Arm, e::R_ARM_TLS_DESC, 8),
            (A::PowerPc64, e::R_PPC64_DTPMOD64, 8),
            (A::PowerPc64, e::R_PPC64_TPREL64, 8),
            (A::PowerPc64, e::R_PPC64_DTPREL64, 8),
            (A::Mips64, e::R_MIPS_TLS_DTPMOD64, 8),
            (A::Mips64, e::R_MIPS_TLS_DTPREL64, 8),
            (A::Mips64, e::R_MIPS_TLS_TPREL64, 8),
        ];
        let mut seen = 0;
        for (arch, r) in word_domain() {
            let want = rows
                .iter()
                .find(|row| row.0 == arch && row.1 == r.r_type && r.r_type2 == e::R_MIPS_NONE)
                .map(|row| {
                    Kind::Compute(
                        Value::Tls,
                        Field::Data {
                            bytes: row.2,
                            signed: false,
                        },
                    )
                });
            seen += usize::from(want.is_some());
            let got = classify(arch, &r);
            if want.is_some() || matches!(got, Some(Kind::Compute(Value::Tls, _))) {
                assert_eq!(got, want, "{arch:?} {r:?}");
            }
        }
        assert_eq!(seen, 32, "16 rows, against a symbol and none");
    }
}
