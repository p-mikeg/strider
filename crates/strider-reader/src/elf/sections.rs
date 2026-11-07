//! Both walks keep one region per loaded start: sections first-wins, segments
//! widest-wins.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::Context as _;
use object::{Object, ObjectKind, ObjectSection, ObjectSegment, ObjectSymbol};

use crate::{FileBytes, MemRegion, Result};

/// Which walk builds the regions.
#[derive(Clone, Copy)]
pub enum RegionSource {
    /// PT_LOAD headers for ET_EXEC / ET_DYN, sections for everything else.
    Auto,
    /// The section walk, even on an image carrying PT_LOAD headers.
    Sections,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoadFilter {
    /// Executable or non-writable mappings: what an instruction fetch may
    /// reference.
    CodeAndReadOnly,
    /// Immutable mappings only: a constant-address load from one folds to its
    /// bytes, so a writable one would fold a store-then-reload to its
    /// file-initial value.
    ImmutableOnly,
    /// [`ImmutableOnly`](Self::ImmutableOnly) plus relocated `.got`, `.got2`
    /// and `.data.rel.ro`, as under `got_is_rom`; a slot only the loader
    /// computes is a hole. Adds none when a tag-placed table is unreadable.
    ImmutableAndRelocatedGot,
    /// Every allocatable mapping, `.data` / `.got` / `.data.rel.ro` included.
    AllAllocatable,
}

impl LoadFilter {
    /// Exec beats write for fetch: firmware can ship a single RWX PT_LOAD.
    fn accepts(self, is_writable: bool, is_exec: bool) -> bool {
        match self {
            LoadFilter::CodeAndReadOnly => is_exec || !is_writable,
            LoadFilter::ImmutableOnly => !is_writable,
            LoadFilter::ImmutableAndRelocatedGot | LoadFilter::AllAllocatable => true,
        }
    }

    fn segment_accepts(self, p_flags: u32) -> bool {
        self.accepts(
            p_flags & object::elf::PF_W != 0,
            p_flags & object::elf::PF_X != 0,
        )
    }

    fn section_accepts(self, sh_flags: u64) -> bool {
        is_mapped(sh_flags)
            && self.accepts(
                has(sh_flags, object::elf::SHF_WRITE),
                has(sh_flags, object::elf::SHF_EXECINSTR),
            )
    }
}

fn sh_flags<'d>(sec: &impl ObjectSection<'d>) -> Option<u64> {
    match sec.flags() {
        object::SectionFlags::Elf { sh_flags } => Some(sh_flags),
        _ => None,
    }
}

fn has(sh_flags: u64, flag: u32) -> bool {
    sh_flags & u64::from(flag) != 0
}

/// `SHF_ALLOC` and not `SHF_TLS`: a section in the flat address space. A
/// TLS section's `sh_addr` is an offset into the per-thread block.
fn is_mapped(sh_flags: u64) -> bool {
    has(sh_flags, object::elf::SHF_ALLOC) && !has(sh_flags, object::elf::SHF_TLS)
}

/// `.got`, `.got2`, `.data.rel.ro` and its suffixed forms; not `.got.plt`.
fn is_relocated_got_name(name: &str) -> bool {
    name == ".got" || name == ".got2" || name == ".data.rel.ro" || name.starts_with(".data.rel.ro.")
}

/// `[start, end)` of every mapped writable section [`is_relocated_got_name`]
/// names, clipped at the next allocated section and the end of its PT_LOAD.
/// Empty when `unread`: an unread table could relocate any of them.
fn relocated_got_ranges(
    obj: &object::File<'_>,
    layout: &ElfSectionLayout,
    unread: bool,
) -> AddressSet {
    if unread {
        return AddressSet::default();
    }
    let mut starts = Vec::new();
    let mut served = Vec::new();
    for sec in obj.sections() {
        let Some(flags) = sh_flags(&sec) else {
            continue;
        };
        if sec.size() == 0 || !is_mapped(flags) {
            continue;
        }
        let base = layout.section_base(&sec);
        starts.push(base);
        if has(flags, object::elf::SHF_WRITE) && sec.name().is_ok_and(is_relocated_got_name) {
            served.push((base, base.saturating_add(sec.size())));
        }
    }
    starts.sort_unstable();
    for (lo, hi) in &mut served {
        if let Some(&next) = starts.get(starts.partition_point(|&s| s <= *lo)) {
            *hi = (*hi).min(next);
        }
        let segment_end = obj
            .segments()
            .map(|seg| (seg.address(), seg.address().saturating_add(seg.size())))
            .find(|&(start, end)| start <= *lo && *lo < end);
        if let Some((_, end)) = segment_end {
            *hi = (*hi).min(end);
        }
    }
    AddressSet::merged(served)
}

/// The mappings one walk accepted, and which addresses are writable.
pub(crate) struct LoadedImage {
    /// One per start.
    pub(crate) regions: Vec<MemRegion>,
    /// Every writable address the walk saw, less the ranges served as ROM.
    pub(crate) writable: AddressSet,
    /// Every executable mapping the walk saw.
    pub(crate) executable: AddressSet,
    /// Indices of the sections the section walk loaded; empty off it.
    pub(crate) owners: BTreeSet<usize>,
}

/// [`LoadedImage`] before its sets are merged.
#[derive(Default)]
struct Walk {
    regions: Vec<MemRegion>,
    writable: Vec<(u64, u64)>,
    executable: Vec<(u64, u64)>,
    owners: BTreeSet<usize>,
}

impl Walk {
    /// Address space, not file bytes: a mapping's BSS tail is writable too.
    fn note_writable(&mut self, start: u64, size: u64) {
        self.writable.push((start, start.saturating_add(size)));
    }

    fn note_executable(&mut self, start: u64, size: u64) {
        self.executable.push((start, start.saturating_add(size)));
    }

    fn into_image(self) -> LoadedImage {
        LoadedImage {
            regions: self.regions,
            writable: AddressSet::merged(self.writable),
            executable: AddressSet::merged(self.executable),
            owners: self.owners,
        }
    }

    /// The image with every writable address but `served`'s cut out of the
    /// regions. `served` also leaves `writable`, so an uncomputed relocation
    /// there becomes a hole.
    fn without_writable_except(self, served: &AddressSet) -> LoadedImage {
        let writable = AddressSet::merged_without(self.writable, served);
        LoadedImage {
            // Overlapping mappings cut at one address leave pieces with equal starts.
            regions: crate::collapse_equal_starts(without_ranges(self.regions, &writable)),
            writable,
            executable: AddressSet::merged(self.executable),
            owners: self.owners,
        }
    }
}

/// `regions` with every address of `ranges` cut out; a split region's pieces
/// share its bytes.
pub fn without_ranges(regions: Vec<MemRegion>, ranges: &AddressSet) -> Vec<MemRegion> {
    let mut kept = Vec::with_capacity(regions.len());
    for region in regions {
        let (start, end) = (region.start_addr(), region.end_addr());
        if ranges.overlapping(start, end).is_empty() {
            kept.push(region);
            continue;
        }
        ranges.pieces_outside(start, end, |lo, hi| kept.push(region.slice(lo, hi)));
    }
    kept
}

/// Non-empty `[start, end)` ranges, ascending, disjoint and non-touching.
#[derive(Clone, Debug, Default)]
pub struct AddressSet(Vec<(u64, u64)>);

impl AddressSet {
    /// `ranges` sorted and merged; `start >= end` is dropped.
    pub fn merged(mut ranges: Vec<(u64, u64)>) -> Self {
        ranges.retain(|&(lo, hi)| lo < hi);
        ranges.sort_unstable();
        let mut out: Vec<(u64, u64)> = Vec::with_capacity(ranges.len());
        for (lo, hi) in ranges {
            match out.last_mut() {
                Some(last) if lo <= last.1 => last.1 = last.1.max(hi),
                _ => out.push((lo, hi)),
            }
        }
        Self(out)
    }

    /// [`merged`](Self::merged) with every address of `cut_out` removed.
    fn merged_without(ranges: Vec<(u64, u64)>, cut_out: &Self) -> Self {
        let merged = Self::merged(ranges);
        let mut out: Vec<(u64, u64)> = Vec::with_capacity(merged.0.len());
        for (lo, hi) in merged.0 {
            cut_out.pieces_outside(lo, hi, |lo, hi| out.push((lo, hi)));
        }
        Self(out)
    }

    /// Each piece of `[lo, hi)` this set does not hold, ascending, to `f`.
    fn pieces_outside(&self, mut lo: u64, hi: u64, mut f: impl FnMut(u64, u64)) {
        for &(c_lo, c_hi) in self.overlapping(lo, hi) {
            if c_lo > lo {
                f(lo, c_lo);
            }
            lo = lo.max(c_hi);
        }
        if lo < hi {
            f(lo, hi);
        }
    }

    /// The ranges overlapping `[lo, hi)`.
    pub(crate) fn overlapping(&self, lo: u64, hi: u64) -> &[(u64, u64)] {
        let first = self.0.partition_point(|&(_, end)| end <= lo);
        let last = first + self.0[first..].partition_point(|&(start, _)| start < hi);
        &self.0[first..last]
    }

    /// The addresses of `self` not in `cut`.
    pub(crate) fn without(&self, cut: &Self) -> Self {
        Self::merged_without(self.0.clone(), cut)
    }

    /// The ranges, ascending.
    pub fn as_slice(&self) -> &[(u64, u64)] {
        &self.0
    }

    /// Whether `[addr, addr + len)` touches any of the ranges.
    pub(crate) fn touches(&self, addr: u64, len: usize) -> bool {
        len != 0
            && !self
                .overlapping(addr, addr.saturating_add(len as u64))
                .is_empty()
    }

    /// Whether the set holds every address of the non-empty `[lo, hi)`.
    pub fn covers(&self, lo: u64, hi: u64) -> bool {
        let i = self.0.partition_point(|&(start, _)| start <= lo);
        i.checked_sub(1).is_some_and(|i| hi <= self.0[i].1)
    }
}

/// Where each ET_REL section is loaded: in index order, an allocated non-TLS
/// section below the watermark moves to `align_up(watermark, sh_addralign)`,
/// starting at a base that leaves 0 unmapped. Identity for a linked image.
/// Independent of the filter, so every view agrees on where a section is.
pub struct ElfSectionLayout(Option<Rebased>);

/// An ET_REL's synthetic link.
struct Rebased {
    /// Section index -> loaded base, for every section header.
    bases: BTreeMap<usize, u64>,
    /// Sections whose base plus declared size overflows: no symbol in them
    /// has an address.
    unseated: BTreeSet<usize>,
    /// Symbol index -> synthetic address of each undefined, `SHN_COMMON` or
    /// defined `STT_GNU_IFUNC` symbol.
    externs: BTreeMap<usize, u64>,
    /// Where the synthetic GOT starts: slot `i` holds symbol `i`'s address.
    got_base: u64,
    /// Bytes per GOT slot.
    word: u64,
    /// The PowerPC64 TOC pointer, `.TOC.`.
    toc_base: Option<u64>,
}

/// Where an ET_REL's first rebased section is seated. Keeps 0 unmapped, so a
/// null dereference does not fold, and fits an absolute 32-bit field.
const ET_REL_IMAGE_BASE: u64 = 0x1000_0000;

/// Granularity of the synthetic ranges past an ET_REL image, which sit one
/// unmapped page apart.
const ET_REL_PAGE: u64 = 0x1000;

/// Address space per undefined or defined-IFUNC symbol in the extern range.
const EXTERN_STRIDE: u64 = 0x10;

/// `.TOC.` sits this far past the start of the TOC it addresses, so a signed
/// 16-bit offset reaches the first 64 KiB of it.
const PPC64_TOC_BIAS: u64 = 0x8000;

impl ElfSectionLayout {
    /// An alignment round-up past `u64::MAX` seats the section at the bare
    /// watermark. A section whose size overflows from its base leaves the
    /// watermark alone and addresses none of its symbols.
    pub fn new(obj: &object::File<'_>) -> Self {
        if obj.kind() != ObjectKind::Relocatable {
            return Self(None);
        }
        let mut bases = BTreeMap::new();
        let mut unseated = BTreeSet::new();
        let mut watermark = ET_REL_IMAGE_BASE;
        let mut toc_section = None;
        for sec in obj.sections() {
            let alloc = sh_flags(&sec).is_some_and(is_mapped);
            // `sh_size`: SHT_NOBITS occupies address space too.
            let size = sec.size();
            let addr = sec.address();
            // An address below the watermark is rebased; an empty section is
            // seated too, moving the watermark to its aligned base.
            let base = if !alloc || addr >= watermark {
                addr
            } else {
                align_up(watermark, sec.align())
            };
            if alloc {
                match base.checked_add(size) {
                    Some(end) => watermark = end,
                    None => {
                        unseated.insert(sec.index().0);
                    }
                }
            }
            if alloc && toc_section.is_none() && sec.name() == Ok(".toc") {
                toc_section = Some(base);
            }
            bases.insert(sec.index().0, base);
        }

        // Past the image: the extern symbols, then the GOT.
        let past = |end: u64| align_up(end, ET_REL_PAGE).saturating_add(ET_REL_PAGE);
        let word = if obj.is_64() { 8 } else { 4 };
        let arch = obj.architecture();
        let mut externs = BTreeMap::new();
        let mut cursor = past(watermark);
        let mut deferred = Vec::new();
        for sym in obj.symbols() {
            if sym.is_common() {
                // `st_value` is the alignment.
                cursor = align_up(cursor, sym.address());
                externs.insert(sym.index().0, cursor);
                cursor = cursor.saturating_add(sym.size().max(1));
            } else if is_ifunc(&sym) && !sym.is_undefined() {
                // Its `st_value` is the resolver.
                cursor = align_up(cursor, EXTERN_STRIDE);
                externs.insert(sym.index().0, cursor);
                cursor = cursor.saturating_add(EXTERN_STRIDE);
            } else if sym.is_undefined() && sym.index().0 != 0 {
                match sym.name() {
                    Ok(".TOC.") if arch == object::Architecture::PowerPc64 => {
                        deferred.push(sym.index().0);
                    }
                    Ok("_GLOBAL_OFFSET_TABLE_") => deferred.push(sym.index().0),
                    _ => {
                        cursor = align_up(cursor, EXTERN_STRIDE);
                        externs.insert(sym.index().0, cursor);
                        cursor = cursor.saturating_add(EXTERN_STRIDE);
                    }
                }
            }
        }
        let got_base = past(cursor);
        let toc_base = (arch == object::Architecture::PowerPc64).then(|| {
            toc_section
                .unwrap_or(got_base)
                .saturating_add(PPC64_TOC_BIAS)
        });
        for index in deferred {
            externs.insert(index, toc_base.unwrap_or(got_base));
        }
        Self(Some(Rebased {
            bases,
            unseated,
            externs,
            got_base,
            word,
            toc_base,
        }))
    }

    /// The synthetic address of an ET_REL undefined, `SHN_COMMON` or defined
    /// `STT_GNU_IFUNC` symbol, in an unmapped range past the image. On
    /// PowerPC64 `.TOC.` and `_GLOBAL_OFFSET_TABLE_` are the TOC pointer,
    /// elsewhere `_GLOBAL_OFFSET_TABLE_` is the GOT's start. `None` otherwise.
    pub fn extern_address(&self, symbol_index: usize) -> Option<u64> {
        self.0.as_ref()?.externs.get(&symbol_index).copied()
    }

    /// Where an ET_REL's synthetic GOT starts.
    pub(crate) fn got_base(&self) -> Option<u64> {
        Some(self.0.as_ref()?.got_base)
    }

    /// The GOT slot holding symbol `symbol_index`'s address, and its width.
    pub(crate) fn got_slot(&self, symbol_index: usize) -> Option<(u64, usize)> {
        let r = self.0.as_ref()?;
        let offset = (symbol_index as u64).checked_mul(r.word)?;
        Some((r.got_base.checked_add(offset)?, r.word as usize))
    }

    /// The PowerPC64 TOC pointer: `0x8000` past the object's `.toc`, else
    /// past the synthetic GOT.
    pub(crate) fn toc_base(&self) -> Option<u64> {
        self.0.as_ref()?.toc_base
    }

    /// Where `sec` is loaded.
    pub fn section_base<'d>(&self, sec: &impl ObjectSection<'d>) -> u64 {
        self.0
            .as_ref()
            .and_then(|r| r.bases.get(&sec.index().0).copied())
            .unwrap_or_else(|| sec.address())
    }

    /// Where `sym` resolves to: an ET_REL `st_value` plus its section's base,
    /// else `st_value`. `None` when an ET_REL `st_shndx` names no section, or
    /// an unseated one.
    pub fn try_symbol_address<'d>(&self, sym: &impl object::ObjectSymbol<'d>) -> Option<u64> {
        let (Some(index), Some(r)) = (sym.section_index(), &self.0) else {
            return Some(sym.address());
        };
        if r.unseated.contains(&index.0) {
            return None;
        }
        Some(r.bases.get(&index.0)?.wrapping_add(sym.address()))
    }

    /// [`try_symbol_address`](Self::try_symbol_address), falling back to the
    /// bare `st_value`; for naming, not patching.
    pub fn symbol_address<'d>(&self, sym: &impl object::ObjectSymbol<'d>) -> u64 {
        self.try_symbol_address(sym)
            .unwrap_or_else(|| sym.address())
    }
}

/// `STT_GNU_IFUNC`, from `st_info`'s low nibble.
pub(crate) fn is_ifunc<'d>(sym: &impl object::ObjectSymbol<'d>) -> bool {
    matches!(
        sym.flags(),
        object::SymbolFlags::Elf { st_info, .. } if st_info & 0xf == object::elf::STT_GNU_IFUNC
    )
}

/// The kind letter of an ARM / AArch64 mapping symbol name: `$a`, `$d`, `$t`
/// or `$x`, optionally suffixed `.<anything>`.
pub fn mapping_symbol_kind(name: &str) -> Option<u8> {
    match name.as_bytes() {
        [b'$', k @ (b'a' | b'd' | b't' | b'x')]
        | [b'$', k @ (b'a' | b'd' | b't' | b'x'), b'.', ..] => Some(*k),
        _ => None,
    }
}

/// The spans of executable sections that ARM / AArch64 mapping symbols mark as
/// data (literal pools): each `$d` run to the next `$a` / `$t` / `$x` in its
/// section, or the section's end. Sorted by start.
pub fn mapping_symbol_data_ranges(obj: &object::File<'_>) -> Vec<std::ops::Range<u64>> {
    use object::{ObjectSymbol, SymbolKind};
    let layout = ElfSectionLayout::new(obj);
    // Per executable section: (address, is data) for every mapping symbol.
    let mut marks: BTreeMap<usize, Vec<(u64, bool)>> = BTreeMap::new();
    for sym in obj.symbols() {
        let (Ok(name), Some(index)) = (sym.name(), sym.section_index()) else {
            continue;
        };
        if sym.kind() == SymbolKind::Section {
            continue;
        }
        let Some(kind) = mapping_symbol_kind(name) else {
            continue;
        };
        let is_data = kind == b'd';
        let Some(address) = layout.try_symbol_address(&sym) else {
            continue;
        };
        marks.entry(index.0).or_default().push((address, is_data));
    }
    let mut out = Vec::new();
    for (index, mut section_marks) in marks {
        let Ok(section) = obj.section_by_index(object::SectionIndex(index)) else {
            continue;
        };
        // By flag, not kind: a debug companion's `.text` is `SHT_NOBITS`.
        let executable = sh_flags(&section).is_some_and(|f| has(f, object::elf::SHF_EXECINSTR));
        if !executable {
            continue;
        }
        let end = layout.section_base(&section).saturating_add(section.size());
        section_marks.sort_unstable();
        // Descending, so a run of `$d` emits one range.
        let mut stop = end;
        let mut run_start: Option<u64> = None;
        for &(start, is_data) in section_marks.iter().rev() {
            if is_data {
                run_start = Some(start);
                continue;
            }
            if let Some(from) = run_start.take()
                && from < stop
            {
                out.push(from..stop);
            }
            stop = start;
        }
        if let Some(from) = run_start
            && from < stop
        {
            out.push(from..stop);
        }
    }
    out.sort_unstable_by_key(|r| r.start);
    out
}

/// Where each ppc64 ELFv1 `.opd` descriptor of an ET_REL enters its code: the
/// `S + A` of the `R_PPC64_ADDR64` on its first word, an unlinked `.opd`
/// holding zeros. Empty for anything else.
pub(crate) fn opd_entries(obj: &object::File<'_>, layout: &ElfSectionLayout) -> BTreeMap<u64, u64> {
    let mut out = BTreeMap::new();
    if obj.architecture() != object::Architecture::PowerPc64
        || obj.kind() != ObjectKind::Relocatable
    {
        return out;
    }
    let (object::File::Elf64(f), Some(opd)) = (obj, obj.section_by_name(".opd")) else {
        return out;
    };
    let base = layout.section_base(&opd);
    for (offset, r_type, sym, addend) in opd_relocations(f, opd.index().0) {
        if r_type != object::elf::R_PPC64_ADDR64 || sym == 0 {
            continue;
        }
        let idx = object::SymbolIndex(sym as usize);
        let Ok(sym) = obj.symbol_by_index(idx) else {
            continue;
        };
        let Some(s) = layout
            .extern_address(idx.0)
            .or_else(|| layout.try_symbol_address(&sym))
        else {
            continue;
        };
        // 0 is never an entry.
        let entry = s.wrapping_add(addend as u64);
        if entry != 0 {
            out.insert(base.wrapping_add(offset), entry);
        }
    }
    out
}

/// The TOC pointer each ppc64 ELFv1 `.opd` descriptor of an ET_REL holds:
/// the `TOC + A` of the `R_PPC64_TOC` on its second word, by descriptor.
fn opd_tocs(obj: &object::File<'_>, layout: &ElfSectionLayout) -> BTreeMap<u64, u64> {
    let (object::File::Elf64(f), Some(opd), Some(toc)) =
        (obj, obj.section_by_name(".opd"), layout.toc_base())
    else {
        return BTreeMap::new();
    };
    let base = layout.section_base(&opd);
    opd_relocations(f, opd.index().0)
        .into_iter()
        .filter(|&(offset, r_type, ..)| r_type == object::elf::R_PPC64_TOC && offset >= 8)
        .map(|(offset, _, _, addend)| {
            (
                base.wrapping_add(offset - 8),
                toc.wrapping_add(addend as u64),
            )
        })
        .collect()
}

/// `(r_offset, r_type, r_sym, r_addend)` of every relocation against the
/// section `target`.
fn opd_relocations(
    f: &object::read::elf::ElfFile64<'_, object::Endianness>,
    target: usize,
) -> Vec<(u64, u32, u32, i64)> {
    use object::read::elf::SectionHeader as _;
    let endian = f.endian();
    let symtab = f.elf_symbol_table().section();
    f.elf_section_table()
        .iter()
        .filter(|sh| {
            matches!(
                sh.sh_type(endian),
                object::elf::SHT_REL | object::elf::SHT_RELA | object::elf::SHT_CREL
            ) && sh.link(endian) == symtab
                && sh.sh_info(endian) as usize == target
        })
        .flat_map(|sh| super::relocations::raw_entries(f, sh))
        .map(|(offset, r_type, sym, addend)| (offset, r_type, sym, addend.unwrap_or(0)))
        .collect()
}

/// ppc64 ELFv1 function descriptors: an `STT_FUNC` `st_value` addresses an
/// {entry, TOC, env} triple in `.opd` whose first doubleword is the code.
pub struct OpdTable<'d> {
    descriptors: Descriptors<'d>,
}

/// Where a descriptor's entry word comes from.
enum Descriptors<'d> {
    /// A linked image's `.opd` file bytes, from `base`.
    Bytes {
        base: u64,
        data: &'d [u8],
        endian_le: bool,
    },
    /// An ET_REL's, from [`opd_entries`] and [`opd_tocs`]: the section's own
    /// bytes are zeros.
    Relocated {
        entries: BTreeMap<u64, u64>,
        tocs: BTreeMap<u64, u64>,
    },
}

impl<'d> OpdTable<'d> {
    /// `None` unless `obj` is a ppc64 image whose `e_flags` ABI level is not
    /// ELFv2 and which carries a non-empty `.opd`.
    pub fn new(obj: &object::File<'d>) -> Option<Self> {
        if obj.architecture() != object::Architecture::PowerPc64 {
            return None;
        }
        // `e_flags` bits 0..1: 2 is ELFv2; 1 and unspecified 0 are ELFv1.
        let object::FileFlags::Elf { e_flags, .. } = obj.flags() else {
            return None;
        };
        if e_flags & 0x3 == 2 {
            return None;
        }
        let opd = obj.section_by_name(".opd")?;
        if obj.kind() == ObjectKind::Relocatable {
            let layout = ElfSectionLayout::new(obj);
            let entries = opd_entries(obj, &layout);
            return (!entries.is_empty()).then(|| Self {
                descriptors: Descriptors::Relocated {
                    entries,
                    tocs: opd_tocs(obj, &layout),
                },
            });
        }
        let data = opd.data().ok()?;
        (data.len() >= 8).then(|| Self {
            descriptors: Descriptors::Bytes {
                base: opd.address(),
                data,
                endian_le: matches!(obj.endianness(), object::Endianness::Little),
            },
        })
    }

    /// The code entry the descriptor at `addr` names; `None` when `addr` is
    /// not an 8-byte-aligned word of the table or the word is zero. On a
    /// linked image any word counts (the 24- or 16-byte stride is unknown), so
    /// a TOC or environment word reads as an entry too.
    pub fn entry_at(&self, addr: u64) -> Option<u64> {
        match &self.descriptors {
            Descriptors::Relocated { entries, .. } => entries.get(&addr).copied(),
            Descriptors::Bytes { .. } => self.word_at(addr),
        }
    }

    /// The TOC pointer, and so `r2` on entry, of the descriptor at `addr`;
    /// `None` where [`Self::entry_at`] is, or when the word is zero.
    pub fn toc_at(&self, addr: u64) -> Option<u64> {
        self.entry_at(addr)?;
        match &self.descriptors {
            Descriptors::Relocated { tocs, .. } => tocs.get(&addr).copied(),
            Descriptors::Bytes { .. } => self.word_at(addr.checked_add(8)?),
        }
    }

    /// A linked table's nonzero 8-byte-aligned word at `addr`.
    fn word_at(&self, addr: u64) -> Option<u64> {
        let Descriptors::Bytes {
            base,
            data,
            endian_le,
        } = self.descriptors
        else {
            return None;
        };
        let off = usize::try_from(addr.checked_sub(base)?).ok()?;
        if off % 8 != 0 {
            return None;
        }
        let word = crate::bytes::read_uint(data.get(off..off.checked_add(8)?)?, endian_le);
        (word != 0).then_some(word)
    }
}

/// `value` rounded up to a multiple of `align`; `align` 0 or 1, or a round-up
/// that would exceed `u64::MAX`, leaves `value` alone.
fn align_up(value: u64, align: u64) -> u64 {
    value
        .checked_next_multiple_of(align.max(1))
        .unwrap_or(value)
}

/// The regions `source` and `filter` select, as windows into `bytes`, which
/// `obj` was parsed from. Errs when an accepted mapping's data can't be read
/// or it ends past `u64::MAX`.
pub(crate) fn collect_regions(
    obj: &object::File<'_>,
    bytes: &FileBytes,
    source: RegionSource,
    filter: LoadFilter,
    layout: &ElfSectionLayout,
    unread: bool,
) -> Result<LoadedImage> {
    let image = match (source, obj.kind()) {
        (RegionSource::Auto, ObjectKind::Executable | ObjectKind::Dynamic) => {
            collect_loadable_segments(obj, bytes, filter)?
        }
        // ET_REL, core and unknown kinds.
        _ => collect_loadable_sections_dedup(obj, bytes, filter, layout)?,
    };
    Ok(match filter {
        // A read-only mapping a writable one overlaps is writable there.
        LoadFilter::ImmutableOnly => image.without_writable_except(&AddressSet::default()),
        LoadFilter::ImmutableAndRelocatedGot => {
            image.without_writable_except(&relocated_got_ranges(obj, layout, unread))
        }
        _ => image.into_image(),
    })
}

/// One mapping, `range` being its file extent, as a window into `bytes`.
/// Errs when it has no file extent or the window does not fit.
fn region_from(bytes: &FileBytes, addr: u64, range: Option<(u64, u64)>) -> Result<MemRegion> {
    let (offset, len) = range
        .ok_or_else(|| anyhow::anyhow!("the mapping at {addr:#x} has bytes but no file extent"))?;
    // A mapping ending exactly at 2^64 loses its last, unmappable byte.
    let tops_out = len != 0 && addr.checked_add(len).is_none() && addr.wrapping_add(len) == 0;
    MemRegion::window(addr, bytes, offset, len - u64::from(tops_out))
}

struct AcceptedSegment {
    addr: u64,
    /// The bytes it both holds and maps.
    file_range: (u64, u64),
}

/// One [`MemRegion`] per accepted PT_LOAD with bytes it both holds and maps,
/// in program-header order, the widest (by that extent) per `p_vaddr`.
fn collect_loadable_segments(
    obj: &object::File<'_>,
    bytes: &FileBytes,
    filter: LoadFilter,
) -> Result<Walk> {
    let mut out = Walk::default();
    let mut accepted: Vec<AcceptedSegment> = Vec::new();
    for seg in obj.segments() {
        let object::SegmentFlags::Elf { p_flags } = seg.flags() else {
            continue;
        };
        if p_flags & object::elf::PF_W != 0 {
            out.note_writable(seg.address(), seg.size());
        }
        if p_flags & object::elf::PF_X != 0 {
            out.note_executable(seg.address(), seg.size());
        }
        if !filter.segment_accepts(p_flags) {
            continue;
        }
        seg.data().context("failed to parse ELF")?;
        // `p_filesz > p_memsz` is undefined; serve only `p_memsz`, the extent
        // `writable` / `executable` track.
        let (file_off, file_len) = seg.file_range();
        let file_len = file_len.min(seg.size());
        if file_len == 0 {
            continue;
        }
        accepted.push(AcceptedSegment {
            addr: seg.address(),
            file_range: (file_off, file_len),
        });
    }

    let mut widest: BTreeMap<u64, usize> = BTreeMap::new();
    for (i, seg) in accepted.iter().enumerate() {
        widest
            .entry(seg.addr)
            // First of a tie.
            .and_modify(|w| {
                if accepted[*w].file_range.1 < seg.file_range.1 {
                    *w = i;
                }
            })
            .or_insert(i);
    }
    let mut keep: Vec<usize> = widest.into_values().collect();
    keep.sort_unstable();

    for i in keep {
        let seg = &accepted[i];
        out.regions
            .push(region_from(bytes, seg.addr, Some(seg.file_range))?);
    }
    Ok(out)
}

/// One [`MemRegion`] per accepted file-backed section, at its
/// [`ElfSectionLayout`] base, in index order, the first per base; the
/// survivors are also [`LoadedImage::owners`]. A `PF_W` PT_LOAD is writable
/// whatever the `sh_flags` of the sections it maps.
fn collect_loadable_sections_dedup(
    obj: &object::File<'_>,
    bytes: &FileBytes,
    filter: LoadFilter,
    layout: &ElfSectionLayout,
) -> Result<Walk> {
    let mut out = Walk::default();
    for seg in obj.segments() {
        if let object::SegmentFlags::Elf { p_flags } = seg.flags()
            && p_flags & object::elf::PF_W != 0
        {
            out.note_writable(seg.address(), seg.size());
        }
    }
    let mut seen = BTreeSet::new();
    for sec in obj.sections() {
        let Some(flags) = sh_flags(&sec) else {
            continue;
        };
        let base = layout.section_base(&sec);
        if is_mapped(flags) {
            if has(flags, object::elf::SHF_WRITE) {
                out.note_writable(base, sec.size());
            }
            if has(flags, object::elf::SHF_EXECINSTR) {
                out.note_executable(base, sec.size());
            }
        }
        if !filter.section_accepts(flags)
            || sec.data().context("failed to parse ELF")?.is_empty()
            || !seen.insert(base)
        {
            continue;
        }
        out.regions
            .push(region_from(bytes, base, sec.file_range())?);
        out.owners.insert(sec.index().0);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use object::elf::{PF_R, PF_W, PF_X};
    use proptest::prelude::*;

    /// An address low in the space or within 80 bytes of its top, so ends
    /// saturate at `u64::MAX`.
    fn address() -> impl Strategy<Value = u64> {
        prop_oneof![0u64..72, (u64::MAX - 80)..=u64::MAX]
    }

    proptest! {
        /// `covers` holds exactly when one range of the set holds the query,
        /// and, for a non-empty query, when every address of it is in the
        /// set: touching, overlapping and empty inputs included.
        #[test]
        fn covers_holds_exactly_when_every_address_is_in_the_set(
            ranges in proptest::collection::vec((address(), 0u64..8), 0..8),
            lo in address(),
            len in 0u64..12,
        ) {
            let ranges: Vec<(u64, u64)> =
                ranges.into_iter().map(|(s, n)| (s, s.saturating_add(n))).collect();
            let hi = lo.saturating_add(len);
            let model: std::collections::BTreeSet<u64> =
                ranges.iter().flat_map(|&(s, e)| s..e).collect();
            let set = AddressSet::merged(ranges);
            let in_one = set.as_slice().iter().any(|&(s, e)| s <= lo && hi <= e);
            prop_assert_eq!(set.covers(lo, hi), in_one);
            if lo < hi {
                prop_assert_eq!(set.covers(lo, hi), (lo..hi).all(|a| model.contains(&a)));
            }
        }
    }

    /// An inverted range holds no address, so it is dropped rather than left
    /// to break the order the queries binary-search.
    #[test]
    fn an_inverted_range_is_dropped() {
        let set = AddressSet::merged(vec![(7, 8), (10, 5)]);
        assert_eq!(set.as_slice(), &[(7, 8)]);
        assert!(!set.covers(5, 10));
    }

    /// An empty range holds no address: it neither touches a query across
    /// it nor splits a region cut by the set.
    #[test]
    fn an_empty_range_is_dropped() {
        let set = AddressSet::merged(vec![(10, 10)]);
        assert!(set.as_slice().is_empty());
        assert!(!set.touches(9, 2));
        let region = MemRegion::new(0, vec![0; 20]).unwrap();
        let kept = without_ranges(vec![region], &set);
        assert_eq!(
            kept.iter()
                .map(|r| (r.start_addr(), r.end_addr()))
                .collect::<Vec<_>>(),
            [(0, 20)]
        );
    }

    /// Each filter over every write / exec combination, the segment and
    /// section walks agreeing. `CodeAndReadOnly` keeps an RWX mapping, the
    /// whole image of firmware or an `ld -N` binary. A section needs
    /// `SHF_ALLOC`, which ET_REL relocation siting relies on, and no
    /// `SHF_TLS`, which `ElfSectionLayout::new` would leave at VMA 0.
    #[test]
    fn each_filter_accepts_by_write_and_exec() {
        let alloc = u64::from(object::elf::SHF_ALLOC);
        let tls = u64::from(object::elf::SHF_TLS);
        // (writable, executable)
        let combos = [(false, false), (false, true), (true, false), (true, true)];
        for (f, want) in [
            (LoadFilter::CodeAndReadOnly, [true, true, false, true]),
            (LoadFilter::ImmutableOnly, [true, true, false, false]),
            (LoadFilter::ImmutableAndRelocatedGot, [true; 4]),
            (LoadFilter::AllAllocatable, [true; 4]),
        ] {
            for ((w, x), want) in combos.into_iter().zip(want) {
                let p_flags = PF_R | if w { PF_W } else { 0 } | if x { PF_X } else { 0 };
                let sh_flags = if w {
                    u64::from(object::elf::SHF_WRITE)
                } else {
                    0
                } | if x {
                    u64::from(object::elf::SHF_EXECINSTR)
                } else {
                    0
                };
                let at = format!("{f:?} w={w} x={x}");
                assert_eq!(f.segment_accepts(p_flags), want, "{at}: segment");
                assert_eq!(f.section_accepts(alloc | sh_flags), want, "{at}: section");
                assert!(!f.section_accepts(sh_flags), "{at}: no SHF_ALLOC");
                assert!(!f.section_accepts(alloc | tls | sh_flags), "{at}: SHF_TLS");
            }
        }
    }
}
