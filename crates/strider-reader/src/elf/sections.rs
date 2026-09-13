//! ELF to [`MemRegion`] loaders.
//!
//! # Dispatch
//!
//! ET_EXEC / ET_DYN walk **program headers** (PT_LOAD), the canonical runtime
//! layout: section headers can be stripped entirely without affecting it.
//!
//! ET_REL and every other kind fall back to walking **sections**, since a
//! relocatable object has no program headers at all (PT_LOAD only appears
//! post-link). Pre-link `sh_addr` is typically 0, so `.text`, `.text.startup`,
//! `.text.foo` commonly share VMA 0. [`ElfSectionLayout`] resolves that the way
//! a linker (and GHIDRA's `.o` import) does, by giving each colliding section
//! its own synthetic base. Every address a caller sees goes through it: a
//! region start, a relocation site, a symbol.
//!
//! Both walks dedup by **loaded** address, since `MemRegionsLookupTable` keeps
//! one region per start: sections first-wins, segments widest-wins. What
//! reaches the section dedup is the non-empty, allocatable, non-TLS sections,
//! and the rebase hands exactly those strictly increasing bases, so on ET_REL
//! it never fires; on a linked image forced down the section walk the bases are
//! the raw `sh_addr`s, where a linker-script overlay can collide. Deciding here
//! keeps the choice deterministic rather than leaving it to that table's
//! last-insert-wins rule.

use std::collections::BTreeMap;

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
    /// `.text`, `.rodata`, `.plt`, `.eh_frame`, plus a writable-but-executable
    /// mapping: what an instruction FETCH may reference.
    CodeAndReadOnly,
    /// Immutable mappings only. The ROM feeds `LoadReadOnly`, which folds a
    /// constant-address load without consulting the memory chain, so a writable
    /// mapping here makes a store-then-reload fold to its file-initial value.
    ImmutableOnly,
    /// Also `.data`, `.got`, `.data.rel.ro`.
    AllAllocatable,
}

impl LoadFilter {
    /// PT_LOAD only. Exec beats write for the fetch role, since a firmware image
    /// can ship a single RWX PT_LOAD.
    fn segment_accepts(self, p_flags: u32) -> bool {
        let is_writable = p_flags & object::elf::PF_W != 0;
        let is_exec = p_flags & object::elf::PF_X != 0;
        match self {
            LoadFilter::CodeAndReadOnly => is_exec || !is_writable,
            LoadFilter::ImmutableOnly => !is_writable,
            LoadFilter::AllAllocatable => true,
        }
    }

    /// Sections always require `SHF_ALLOC` and reject `SHF_TLS`; `SHF_WRITE` /
    /// `SHF_EXECINSTR` then pick exec-or-rodata vs include-writable.
    ///
    /// A `SHF_TLS` section is addressed as an offset into the per-thread block,
    /// so [`ElfSectionLayout`] leaves it at its `sh_addr`. Loading it would map
    /// a non-empty `.tdata` at its `sh_addr`, 0 in a `.o`, at an address that
    /// means nothing and that the rebase deliberately leaves unmapped.
    fn section_accepts(self, sh_flags: u64) -> bool {
        let is_alloc = sh_flags & u64::from(object::elf::SHF_ALLOC) != 0;
        let is_tls = sh_flags & u64::from(object::elf::SHF_TLS) != 0;
        if !is_alloc || is_tls {
            return false;
        }
        let is_exec = sh_flags & u64::from(object::elf::SHF_EXECINSTR) != 0;
        let is_writable = sh_flags & u64::from(object::elf::SHF_WRITE) != 0;
        match self {
            LoadFilter::CodeAndReadOnly => is_exec || !is_writable,
            LoadFilter::ImmutableOnly => !is_writable,
            LoadFilter::AllAllocatable => true,
        }
    }
}

/// The instruction-FETCH image, kind-dispatched per the module docs: PT_LOAD
/// program headers for ET_EXEC / ET_DYN, sections at their
/// [`ElfSectionLayout`] bases otherwise.
///
/// A writable-but-executable mapping is included, since a firmware image can
/// ship a single RWX PT_LOAD and there would otherwise be nothing to decode.
/// That makes this a superset of the runtime-immutable image;
/// [`super::ElfFileMemReader`] subtracts the writable ones from it for its
/// [`crate::ReadOnlyMemory`] view.
///
/// # Errors
///
/// When an accepted segment or section's `data()` can't be read, its
/// `address + length` would exceed `u64::MAX`, or the copies exceed the
/// loader's amplification ceiling over the distinct file bytes behind them;
/// every mapping is copied here, the caller holding no image to window into.
pub fn elf_get_loadable_regions(obj: &object::File<'_>) -> Result<Vec<MemRegion>> {
    Ok(collect_regions(
        obj,
        None,
        RegionSource::Auto,
        LoadFilter::CodeAndReadOnly,
        &ElfSectionLayout::new(obj),
    )?
    .regions)
}

/// The mappings one walk accepted, and which addresses are writable.
#[derive(Default)]
pub(crate) struct LoadedImage {
    pub(crate) regions: Vec<MemRegion>,
    /// Every writable mapping the walk SAW, whether or not it survived the
    /// filter and the dedup: a writable PT_LOAD overlapping an accepted
    /// read-only one leaves no region behind, and a [`crate::ReadOnlyMemory`]
    /// view that did not know about it would serve the accepted mapping's
    /// file-initial bytes for an address that is RW at runtime.
    pub(crate) writable: AddressRanges,
}

impl LoadedImage {
    /// Address space, not file bytes: a mapping's BSS tail is writable too.
    fn note_writable(&mut self, start: u64, size: u64) {
        if size != 0 {
            self.writable.0.push((start, start.saturating_add(size)));
        }
    }
}

/// `[start, end)` address ranges, ascending and disjoint once
/// [`merged`](Self::merged).
#[derive(Clone, Debug, Default)]
pub(crate) struct AddressRanges(Vec<(u64, u64)>);

impl AddressRanges {
    /// `ranges` sorted, with everything that overlaps or touches merged.
    pub(crate) fn merged(mut ranges: Vec<(u64, u64)>) -> Self {
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

    /// The merged ranges overlapping `[lo, hi)`.
    pub(crate) fn overlapping(&self, lo: u64, hi: u64) -> &[(u64, u64)] {
        let first = self.0.partition_point(|&(_, end)| end <= lo);
        let last = first + self.0[first..].partition_point(|&(start, _)| start < hi);
        &self.0[first..last]
    }

    /// Whether `[addr, addr + len)` touches any of the merged ranges.
    pub(crate) fn touches(&self, addr: u64, len: usize) -> bool {
        len != 0
            && !self
                .overlapping(addr, addr.saturating_add(len as u64))
                .is_empty()
    }
}

/// One accepted section, carrying everything the section walks read.
struct AcceptedSection<'d> {
    index: usize,
    base: u64,
    file_range: Option<(u64, u64)>,
    data: &'d [u8],
}

/// Every section the section walk accepts under `filter`, in index order.
///
/// Which sections survive decides where regions land and which relocation sites
/// this crate owns. Both have to agree, so they read this one walk instead of
/// each spelling the filter, the empty-section skip and the layout base out
/// again.
///
/// `layout` must be the one built for `obj`.
///
/// # Errors
///
/// When an accepted section's `data()` can't be read.
fn accepted_sections<'d>(
    obj: &object::File<'d>,
    filter: LoadFilter,
    layout: &ElfSectionLayout,
) -> Result<Vec<AcceptedSection<'d>>> {
    let mut out = Vec::new();
    for sec in obj.sections() {
        let object::read::SectionFlags::Elf { sh_flags } = sec.flags() else {
            continue;
        };
        if !filter.section_accepts(sh_flags) {
            continue;
        }
        let data = sec.data().context("failed to parse ELF")?;
        if data.is_empty() {
            continue;
        }
        out.push(AcceptedSection {
            index: sec.index().0,
            base: layout.section_base(&sec),
            file_range: sec.file_range(),
            data,
        });
    }
    Ok(out)
}

/// [`accepted_sections`] under **first-wins dedup** on the loaded base, still in
/// index order.
///
/// # Errors
///
/// Same as [`accepted_sections`].
fn deduped_sections<'d>(
    obj: &object::File<'d>,
    filter: LoadFilter,
    layout: &ElfSectionLayout,
) -> Result<Vec<AcceptedSection<'d>>> {
    let mut seen: std::collections::BTreeSet<u64> = std::collections::BTreeSet::new();
    Ok(accepted_sections(obj, filter, layout)?
        .into_iter()
        .filter(|sec| seen.insert(sec.base))
        .collect())
}

/// Where each ET_REL section is loaded: a synthetic image base upwards, so
/// address 0 stays unmapped.
///
/// An ET_REL object is unlinked, and every toolchain leaves `sh_addr` at 0
/// there, so `.text`, `.text.startup` and `.rodata` all claim VMA 0. Assigning
/// them distinct addresses is the linker's job, and this does it: sections are
/// walked in index order and any whose `sh_addr` runs into already-placed space
/// moves up to `align_up(watermark, sh_addralign)`, the watermark starting at
/// the image base.
///
/// Populated only for an ET_REL. A linked image has real, disjoint `sh_addr`s,
/// so it holds no entries and every address passes through untouched.
/// The layout is filter-independent (every section participates, loaded or
/// not), so the fetch and ROM views agree on where a section is even when only
/// one of them maps it.
pub struct ElfSectionLayout {
    /// Section index -> loaded base. Absent means the section's own `sh_addr`.
    bases: BTreeMap<usize, u64>,
    /// Sections whose base plus declared size runs past the address space: the
    /// base is where their region would go, and nothing can be addressed
    /// through it.
    unseated: std::collections::BTreeSet<usize>,
    /// ET_REL, so `bases` holds every section header and an index absent from
    /// it is out of range rather than a pass-through.
    rebased: bool,
    /// Symbol index -> synthetic address, for every ET_REL symbol the link
    /// would place outside this object: undefined and `SHN_COMMON` ones.
    externs: BTreeMap<usize, u64>,
    /// Where the synthetic GOT starts: slot `i` holds symbol `i`'s address.
    got_base: u64,
    /// Bytes per GOT slot.
    word: u64,
    /// The PowerPC64 TOC pointer, `.TOC.`.
    toc_base: Option<u64>,
}

/// Where an ET_REL's first rebased section is seated, so that address 0 stays
/// unmapped: a mapped 0 makes a null dereference a readable, foldable ROM read.
///
/// Round, obviously not a link-time address, and inside the low 2 GiB so an
/// absolute 32-bit relocation field still holds it.
const ET_REL_IMAGE_BASE: u64 = 0x1000_0000;

/// Page granularity of the synthetic ranges past an ET_REL image: each starts
/// on a page boundary one unmapped page past the previous one.
const ET_REL_PAGE: u64 = 0x1000;

/// Address space per undefined symbol in the extern range.
const EXTERN_STRIDE: u64 = 0x10;

/// `.TOC.` sits this far past the start of the TOC it addresses, so a signed
/// 16-bit offset reaches the first 64 KiB of it.
const PPC64_TOC_BIAS: u64 = 0x8000;

impl ElfSectionLayout {
    /// A rebase whose alignment round-up would run past `u64::MAX` seats the
    /// section at the bare watermark; building its [`MemRegion`] is what
    /// reports an `address + length` overflow. A section whose declared size
    /// runs past `u64::MAX` from its base leaves the watermark alone and
    /// addresses none of its symbols.
    pub fn new(obj: &object::File<'_>) -> Self {
        let mut bases = BTreeMap::new();
        let mut unseated = std::collections::BTreeSet::new();
        // A linked image's section addresses are the real ones.
        if obj.kind() != ObjectKind::Relocatable {
            return Self {
                bases,
                unseated,
                rebased: false,
                externs: BTreeMap::new(),
                got_base: 0,
                word: 0,
                toc_base: None,
            };
        }
        let mut watermark = ET_REL_IMAGE_BASE;
        let mut toc_section = None;
        for sec in obj.sections() {
            // `SHF_TLS` is allocatable but lives in the per-thread block, not
            // the flat address space: a `.tdata` / `.tbss` symbol's `st_value`
            // is an offset into that block, so giving the section a base here
            // would mint an address that means nothing.
            let alloc = matches!(
                sec.flags(),
                object::read::SectionFlags::Elf { sh_flags }
                    if sh_flags & u64::from(object::elf::SHF_ALLOC) != 0
                        && sh_flags & u64::from(object::elf::SHF_TLS) == 0
            );
            // `sh_size`, not the file bytes: SHT_NOBITS (`.bss`, `.tbss`) has
            // no bytes yet still occupies address space, and its symbols
            // resolve through this base.
            let size = sec.size();
            let addr = sec.address();
            // A non-allocatable or zero-size section occupies no address
            // space, so it stays at its `sh_addr`. The watermark is monotone,
            // so a section DECLARING an address below it is rebased even
            // though that `sh_addr` is authoritative; gcc/clang/`ld -r` emit 0
            // for every ET_REL section, which is what makes the rule safe on
            // toolchain output and only a risk on a hand-crafted object.
            let base = if !alloc || size == 0 || addr >= watermark {
                addr
            } else {
                align_up(watermark, sec.align())
            };
            // A `sh_size` overflowing the address space is malformed and no
            // base holds it. The watermark stays put, everywhere the sections
            // after it could go being inside the range this one claims, and
            // its symbols get no address rather than one resolving into
            // whichever section took their offsets. SHT_NOBITS never validates
            // `sh_size`, having no file bytes to bound it; a section carrying
            // bytes fails the load where its region is built.
            if alloc && size != 0 {
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

        // Past the image, one unmapped page apart: the undefined and common
        // symbols, then the GOT.
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
        Self {
            bases,
            unseated,
            rebased: true,
            externs,
            got_base,
            word,
            toc_base,
        }
    }

    /// The address an ET_REL gives a symbol the link would place outside the
    /// object: an undefined symbol gets a distinct one in an unmapped range
    /// past the image, a `SHN_COMMON` one room for its size there. `.TOC.` is
    /// the PowerPC64 TOC pointer and `_GLOBAL_OFFSET_TABLE_` the GOT's start.
    ///
    /// `None` for a defined symbol and for any symbol of a linked image.
    pub fn extern_address(&self, symbol_index: usize) -> Option<u64> {
        self.externs.get(&symbol_index).copied()
    }

    /// Where the synthetic GOT starts, `_GLOBAL_OFFSET_TABLE_`.
    pub(crate) fn got_base(&self) -> u64 {
        self.got_base
    }

    /// The GOT slot holding symbol `symbol_index`'s address, and its width.
    pub(crate) fn got_slot(&self, symbol_index: usize) -> Option<(u64, usize)> {
        let offset = (symbol_index as u64).checked_mul(self.word)?;
        Some((self.got_base.checked_add(offset)?, self.word as usize))
    }

    /// The PowerPC64 TOC pointer: `.TOC.`, `0x8000` past the object's `.toc`.
    pub(crate) fn toc_base(&self) -> Option<u64> {
        self.toc_base
    }

    /// Whether the layout rebased an ET_REL.
    pub(crate) fn is_rebased(&self) -> bool {
        self.rebased
    }

    /// Where `sec` is loaded.
    pub fn section_base<'d>(&self, sec: &impl ObjectSection<'d>) -> u64 {
        self.base(sec.index().0).unwrap_or_else(|| sec.address())
    }

    /// Where `sym` resolves to. gABI: an ET_REL `st_value` is an offset from
    /// the start of the section `st_shndx` names, so the address is that
    /// section's base plus it; a linked image's `st_value` is already the
    /// address and no base is recorded. An undefined, absolute or `SHN_COMMON`
    /// symbol has no section index and is returned as-is.
    ///
    /// `None` when an ET_REL `st_shndx` names no section header, or one whose
    /// declared size the address space cannot hold: the offset it declares has
    /// no base, so the symbol has no address.
    pub fn try_symbol_address<'d>(&self, sym: &impl object::ObjectSymbol<'d>) -> Option<u64> {
        let Some(index) = sym.section_index() else {
            return Some(sym.address());
        };
        if self.unseated.contains(&index.0) {
            return None;
        }
        match self.base(index.0) {
            Some(base) => Some(base.wrapping_add(sym.address())),
            None if self.rebased => None,
            None => Some(sym.address()),
        }
    }

    /// [`try_symbol_address`], reading an out-of-range `st_shndx` as the bare
    /// `st_value`. Naming a symbol is what that serves; patching one is
    /// [`try_symbol_address`]'s.
    ///
    /// [`try_symbol_address`]: Self::try_symbol_address
    pub fn symbol_address<'d>(&self, sym: &impl object::ObjectSymbol<'d>) -> u64 {
        self.try_symbol_address(sym)
            .unwrap_or_else(|| sym.address())
    }

    fn base(&self, section_index: usize) -> Option<u64> {
        self.bases.get(&section_index).copied()
    }
}

/// ppc64 ELFv1 function descriptors.
///
/// On that ABI `st_value` of an `STT_FUNC` symbol addresses an 8-byte-aligned
/// {entry, TOC, env} triple in `.opd` rather than code, and the first
/// doubleword is the address to decode from. ELFv2 has no descriptors, and no
/// other architecture defines them.
pub struct OpdTable<'d> {
    base: u64,
    data: &'d [u8],
    endian_le: bool,
}

impl<'d> OpdTable<'d> {
    /// `None` unless `obj` is a linked ppc64 image whose `e_flags` ABI level
    /// is not ELFv2 and which carries an `.opd`.
    ///
    /// The descriptor CONTENTS are the section's file bytes, which is why
    /// ET_REL is excluded: an unlinked `.opd` holds zeros until its
    /// `R_PPC64_ADDR64` relocations are applied.
    pub fn new(obj: &object::File<'d>) -> Option<Self> {
        if obj.architecture() != object::Architecture::PowerPc64
            || obj.kind() == ObjectKind::Relocatable
        {
            return None;
        }
        // `e_flags` bits 0..1 are the ABI level. 2 is ELFv2; 1 is ELFv1, and
        // so is the 0 that means "unspecified", which is what a toolchain
        // emitting an `.opd` at all has to mean.
        let object::FileFlags::Elf { e_flags, .. } = obj.flags() else {
            return None;
        };
        if e_flags & 0x3 == 2 {
            return None;
        }
        let opd = obj.section_by_name(".opd")?;
        let data = opd.data().ok()?;
        (data.len() >= 8).then(|| Self {
            base: opd.address(),
            data,
            endian_le: matches!(obj.endianness(), object::Endianness::Little),
        })
    }

    /// The code entry the descriptor at `addr` names, or `None` when `addr` is
    /// not a whole descriptor word in this table, or when the word reads zero.
    ///
    /// "Whole word" is the 8-byte stride the descriptor triple is built from:
    /// a `.opd` offset that is not a multiple of it straddles the {entry, TOC}
    /// boundary and would read half of each as one address. A multiple naming
    /// a descriptor's TOC or environment word is followed as if it were an
    /// entry: which words start a descriptor takes a stride the section does
    /// not carry, the triple being 24 bytes in compiler output and 16 in the
    /// hand-written asm that leaves the environment word off.
    ///
    /// A zero word is a descriptor nothing filled in: address 0 is never the
    /// entry, and passing `addr` through unfollowed leaves the descriptor
    /// visible.
    pub fn entry_at(&self, addr: u64) -> Option<u64> {
        let off = usize::try_from(addr.checked_sub(self.base)?).ok()?;
        if off % 8 != 0 {
            return None;
        }
        let word: [u8; 8] = self.data.get(off..off.checked_add(8)?)?.try_into().ok()?;
        let entry = if self.endian_le {
            u64::from_le_bytes(word)
        } else {
            u64::from_be_bytes(word)
        };
        (entry != 0).then_some(entry)
    }
}

/// `value` rounded up to a multiple of `align`; `align` 0 or 1, or a round-up
/// that would exceed `u64::MAX`, leaves `value` alone.
fn align_up(value: u64, align: u64) -> u64 {
    value
        .checked_next_multiple_of(align.max(1))
        .unwrap_or(value)
}

/// Which sections the ET_REL relocation walk owns, i.e. exactly those
/// [`collect_loadable_sections_dedup`] kept.
///
/// Shares [`deduped_sections`] with it rather than inferring the set from the
/// loaded bytes: which sections survive depends on `filter`, and two sections
/// holding equal bytes are indistinguishable afterwards, so guessing would
/// write `.rela.data` straight over `.text.f`.
///
/// `filter` must be the one the regions were loaded with, and `layout` the one
/// built for `obj`.
///
/// Empty for every kind but ET_REL, the only one whose relocations are sited
/// through sections.
///
/// # Errors
///
/// When an allocatable section's `data()` can't be read, matching
/// [`collect_loadable_sections_dedup`].
pub(crate) fn loaded_section_indices(
    obj: &object::File<'_>,
    layout: &ElfSectionLayout,
    filter: LoadFilter,
) -> Result<std::collections::BTreeSet<usize>> {
    if obj.kind() != ObjectKind::Relocatable {
        return Ok(std::collections::BTreeSet::new());
    }
    Ok(deduped_sections(obj, filter, layout)?
        .into_iter()
        .map(|sec| sec.index)
        .collect())
}

/// `bytes`, when given, is the whole image the regions are windows into; the
/// regions then share it instead of copying. `None` copies each mapping, for a
/// caller holding only a parsed [`object::File`].
///
/// # Errors
///
/// When an accepted segment or section's `data()` can't be read, its
/// `address + length` would exceed `u64::MAX`, or, on the copying path, the
/// copies exceed [`MAX_COPY_AMPLIFICATION`] times the distinct file bytes
/// behind them.
pub(crate) fn collect_regions(
    obj: &object::File<'_>,
    bytes: Option<&FileBytes>,
    source: RegionSource,
    filter: LoadFilter,
    layout: &ElfSectionLayout,
) -> Result<LoadedImage> {
    let image = match (source, obj.kind()) {
        (RegionSource::Auto, ObjectKind::Executable | ObjectKind::Dynamic) => {
            collect_loadable_segments(obj, bytes, filter)?
        }
        // ET_REL plus any unknown / core kind. An `.o` has no program headers,
        // and a core dump's segment layout isn't what the analyser wants
        // either, so the section walk is the safer fallback for both.
        _ => collect_loadable_sections_dedup(obj, bytes, filter, layout)?,
    };
    Ok(LoadedImage {
        writable: AddressRanges::merged(image.writable.0),
        ..image
    })
}

/// Ceiling on the bytes the copying path materialises, as a multiple of the
/// distinct file bytes it has been asked to copy.
const MAX_COPY_AMPLIFICATION: u64 = 4;

/// What the copying path has allocated, against the file bytes it has seen.
///
/// Headers name a file extent and an address independently, so N mappings can
/// each copy one blob: `sh_size` is bounded by the file, their sum is not. A
/// crafted 8 MB image with 65k allocatable section headers over one 4 MB blob
/// otherwise asks for a quarter of a terabyte and aborts.
///
/// The denominator is the merged UNION of those extents, not a set of
/// `(offset, size)` keys: N windows over one blob at N consecutive offsets are
/// N distinct keys but cover blob + N bytes, so keys would let the copies grow
/// in lockstep with the denominator and the ratio never fire.
#[derive(Default)]
struct CopyBudget {
    copied: u64,
    /// Disjoint, non-touching `[start, end)` file extents, keyed by start.
    covered: BTreeMap<u64, u64>,
    covered_total: u64,
}

impl CopyBudget {
    /// # Errors
    ///
    /// When the copies exceed [`MAX_COPY_AMPLIFICATION`] times the distinct
    /// file bytes behind them.
    fn charge(&mut self, range: Option<(u64, u64)>, len: u64) -> Result<()> {
        let fresh = match range {
            Some((offset, extent)) => self.cover(offset, extent),
            // No file extent to attribute the bytes to, so they are their own
            // denominator.
            None => len,
        };
        self.covered_total = self.covered_total.saturating_add(fresh);
        self.copied = self.copied.saturating_add(len);
        if self.copied > self.covered_total.saturating_mul(MAX_COPY_AMPLIFICATION) {
            anyhow::bail!(
                "loading would copy {} bytes out of {} distinct file bytes",
                self.copied,
                self.covered_total
            );
        }
        Ok(())
    }

    /// Adds `[offset, offset + len)` to the union, absorbing every extent it
    /// overlaps or touches, and answers the bytes the union gained.
    fn cover(&mut self, offset: u64, len: u64) -> u64 {
        let Some(end) = offset.checked_add(len).filter(|_| len != 0) else {
            return 0;
        };
        let (mut lo, mut hi, mut absorbed) = (offset, end, 0u64);
        // Descending from the last extent starting at or before `hi`: extents
        // are disjoint, so the first one ending below `lo` ends the run.
        while let Some((&start, &stop)) = self.covered.range(..=hi).next_back() {
            if stop < lo {
                break;
            }
            (lo, hi) = (lo.min(start), hi.max(stop));
            absorbed += stop - start;
            self.covered.remove(&start);
        }
        self.covered.insert(lo, hi);
        (hi - lo) - absorbed
    }
}

/// One mapping of `data`, as a window into `bytes` when the image is at hand
/// and `range` is its file-backed extent, else as an owned copy charged to
/// `budget`.
fn region_from(
    bytes: Option<&FileBytes>,
    addr: u64,
    range: Option<(u64, u64)>,
    data: &[u8],
    budget: &mut CopyBudget,
) -> Result<MemRegion> {
    match (bytes, range) {
        (Some(bytes), Some((offset, len))) => MemRegion::window(addr, bytes, offset, len),
        _ => {
            budget.charge(range, data.len() as u64)?;
            MemRegion::new(addr, data.to_vec())
        }
    }
}

/// One accepted PT_LOAD, carrying everything the segment walk reads.
struct AcceptedSegment<'d> {
    addr: u64,
    file_range: (u64, u64),
    data: &'d [u8],
}

/// One [`MemRegion`] per accepted PT_LOAD segment, from its file-backed bytes,
/// in program-header order under **widest-wins dedup** on `p_vaddr`.
/// Empty `data()` (a BSS-only segment, `p_filesz == 0`) has nothing to load and
/// is skipped.
///
/// The dedup rule is the section walk's, resolved the other way: only one
/// region per start address survives [`crate::MemRegionsLookupTable`], so
/// keeping a narrower mapping would leave the bytes past its end unfetchable
/// even though a wider mapping declares them.
fn collect_loadable_segments<'d>(
    obj: &object::File<'d>,
    bytes: Option<&FileBytes>,
    filter: LoadFilter,
) -> Result<LoadedImage> {
    let mut out = LoadedImage::default();
    let mut accepted: Vec<AcceptedSegment<'d>> = Vec::new();
    for seg in obj.segments() {
        // `obj.segments()` already yields PT_LOAD only, so `p_flags` is read
        // purely for the writable / executable filter axis.
        let object::SegmentFlags::Elf { p_flags } = seg.flags() else {
            continue;
        };
        if p_flags & object::elf::PF_W != 0 {
            out.note_writable(seg.address(), seg.size());
        }
        if !filter.segment_accepts(p_flags) {
            continue;
        }
        let data = seg.data().context("failed to parse ELF")?;
        if data.is_empty() {
            continue;
        }
        accepted.push(AcceptedSegment {
            addr: seg.address(),
            file_range: seg.file_range(),
            data,
        });
    }

    let mut widest: BTreeMap<u64, usize> = BTreeMap::new();
    for (i, seg) in accepted.iter().enumerate() {
        widest
            .entry(seg.addr)
            // First of a tie, so an image with no collisions is untouched.
            .and_modify(|w| {
                if accepted[*w].data.len() < seg.data.len() {
                    *w = i;
                }
            })
            .or_insert(i);
    }
    let mut keep: Vec<usize> = widest.into_values().collect();
    keep.sort_unstable();

    let mut budget = CopyBudget::default();
    for i in keep {
        let seg = &accepted[i];
        out.regions.push(region_from(
            bytes,
            seg.addr,
            Some(seg.file_range),
            seg.data,
            &mut budget,
        )?);
    }
    Ok(out)
}

/// One [`MemRegion`] per accepted file-backed section, at its
/// [`ElfSectionLayout`] base, in section-index order under **first-wins dedup**
/// on that base.
fn collect_loadable_sections_dedup(
    obj: &object::File<'_>,
    bytes: Option<&FileBytes>,
    filter: LoadFilter,
    layout: &ElfSectionLayout,
) -> Result<LoadedImage> {
    let mut out = LoadedImage::default();
    for sec in obj.sections() {
        let object::read::SectionFlags::Elf { sh_flags } = sec.flags() else {
            continue;
        };
        let bit = |flag: u32| sh_flags & u64::from(flag) != 0;
        // `SHF_TLS` is excluded for the reason [`ElfSectionLayout::new`] gives:
        // its `sh_addr` names nothing in the flat address space.
        if bit(object::elf::SHF_ALLOC) && bit(object::elf::SHF_WRITE) && !bit(object::elf::SHF_TLS)
        {
            out.note_writable(layout.section_base(&sec), sec.size());
        }
    }
    let mut budget = CopyBudget::default();
    for sec in deduped_sections(obj, filter, layout)? {
        out.regions.push(region_from(
            bytes,
            sec.base,
            sec.file_range,
            sec.data,
            &mut budget,
        )?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use object::elf::{PF_R, PF_W, PF_X};

    /// An RWX PT_LOAD is the whole image on firmware / bare-metal / `ld -N`
    /// binaries. Excluding it leaves the instruction-fetch reader with no
    /// regions, reported as "address is not mapped" on the first decode.
    #[test]
    fn code_and_read_only_keeps_an_executable_writable_segment() {
        let f = LoadFilter::CodeAndReadOnly;
        assert!(f.segment_accepts(PF_R | PF_W | PF_X), "RWX is still code");
        assert!(f.segment_accepts(PF_R | PF_X), "RX");
        assert!(f.segment_accepts(PF_R), "RO");
        assert!(
            !f.segment_accepts(PF_R | PF_W),
            "RW data stays out of the ROM"
        );
    }

    /// The segment and section filters must agree on the exec-over-write rule,
    /// since the same `LoadFilter` serves both walks.
    #[test]
    fn segment_and_section_filters_agree_on_exec_over_write() {
        let f = LoadFilter::CodeAndReadOnly;
        let alloc = u64::from(object::elf::SHF_ALLOC);
        let write = u64::from(object::elf::SHF_WRITE);
        let exec = u64::from(object::elf::SHF_EXECINSTR);
        assert_eq!(
            f.segment_accepts(PF_R | PF_W | PF_X),
            f.section_accepts(alloc | write | exec)
        );
        assert_eq!(
            f.segment_accepts(PF_R | PF_W),
            f.section_accepts(alloc | write)
        );
    }

    #[test]
    fn all_allocatable_takes_everything() {
        let f = LoadFilter::AllAllocatable;
        assert!(f.segment_accepts(PF_R | PF_W));
        assert!(f.section_accepts(u64::from(object::elf::SHF_ALLOC)));
    }

    /// "Allocatable" is the floor for every section filter, so membership in
    /// [`loaded_section_indices`] already implies `SHF_ALLOC`. ET_REL relocation
    /// siting relies on that to avoid a second alloc check.
    #[test]
    fn every_section_filter_requires_alloc() {
        let write = u64::from(object::elf::SHF_WRITE);
        let exec = u64::from(object::elf::SHF_EXECINSTR);
        for f in [
            LoadFilter::CodeAndReadOnly,
            LoadFilter::ImmutableOnly,
            LoadFilter::AllAllocatable,
        ] {
            assert!(!f.section_accepts(0), "no flags at all");
            assert!(!f.section_accepts(write | exec), "SHF_ALLOC absent");
        }
    }

    /// [`ElfSectionLayout::new`] leaves a `SHF_TLS` section at its `sh_addr`,
    /// so a filter that loaded one would seat `.tdata` at VMA 0 on top of
    /// `.text`. Every filter must reject it, `AllAllocatable` included.
    #[test]
    fn no_section_filter_accepts_a_tls_section() {
        let alloc = u64::from(object::elf::SHF_ALLOC);
        let write = u64::from(object::elf::SHF_WRITE);
        let tls = u64::from(object::elf::SHF_TLS);
        for f in [
            LoadFilter::CodeAndReadOnly,
            LoadFilter::ImmutableOnly,
            LoadFilter::AllAllocatable,
        ] {
            assert!(!f.section_accepts(alloc | write | tls), ".tdata");
            assert!(!f.section_accepts(alloc | tls), "SHF_TLS without SHF_WRITE");
            assert!(f.section_accepts(alloc), "a plain allocatable section");
        }
    }
}
