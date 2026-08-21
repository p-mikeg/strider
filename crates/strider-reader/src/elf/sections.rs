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
//! Section collection still dedups by **loaded** address, first-wins. After the
//! rebase that can only fire on a linked image forced down the section walk
//! (`.tbss` overlapping `.tdata`), where dropping the later section keeps the
//! choice deterministic rather than leaving it to `MemRegionsLookupTable`'s
//! last-insert-wins rule.

use std::collections::BTreeMap;

use anyhow::Context as _;
use object::{Object, ObjectKind, ObjectSection, ObjectSegment};

use crate::{FileBytes, MemRegion, Result};

/// Which walk builds the regions.
#[derive(Clone, Copy)]
pub enum RegionSource {
    /// PT_LOAD headers for ET_EXEC / ET_DYN, sections for everything else.
    Auto,
    /// The section walk, even on an image carrying PT_LOAD headers.
    Sections,
}

#[derive(Clone, Copy)]
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

    /// Sections always require `SHF_ALLOC`; `SHF_WRITE` / `SHF_EXECINSTR` then
    /// pick exec-or-rodata vs include-writable.
    fn section_accepts(self, sh_flags: u64) -> bool {
        let is_alloc = sh_flags & u64::from(object::elf::SHF_ALLOC) != 0;
        if !is_alloc {
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
/// That makes this a superset of the runtime-immutable image; a
/// [`crate::ReadOnlyMemory`] view must come from [`elf_get_readonly_regions`]
/// instead.
///
/// # Errors
///
/// When an accepted segment or section's `data()` can't be read, or its
/// `address + length` would exceed `u64::MAX`.
pub fn elf_get_loadable_regions(obj: &object::File<'_>) -> Result<Vec<MemRegion>> {
    collect_regions(obj, None, RegionSource::Auto, LoadFilter::CodeAndReadOnly)
}

/// The runtime-immutable image: [`elf_get_loadable_regions`] minus every
/// writable mapping, RWX included. Same dispatch.
///
/// # Errors
///
/// Same as [`elf_get_loadable_regions`].
pub fn elf_get_readonly_regions(obj: &object::File<'_>) -> Result<Vec<MemRegion>> {
    collect_regions(obj, None, RegionSource::Auto, LoadFilter::ImmutableOnly)
}

/// A strict superset of [`elf_get_loadable_regions`], adding writable mappings
/// (`.data.rel.ro`, `.got.plt`, `.data`). Same dispatch.
///
/// # Errors
///
/// Same as [`elf_get_loadable_regions`].
pub fn elf_get_loadable_regions_including_writable(
    obj: &object::File<'_>,
) -> Result<Vec<MemRegion>> {
    collect_regions(obj, None, RegionSource::Auto, LoadFilter::AllAllocatable)
}

/// `[start, end)` of every mapping [`elf_get_loadable_regions`] accepts that is
/// NOT immutable, i.e. a writable-but-executable one. Subtracting these from the
/// fetch image gives the `ReadOnlyMemory` view, without a second copy of the
/// bytes: only lengths are read here.
///
/// # Errors
///
/// Same as [`elf_get_loadable_regions`].
pub(crate) fn elf_writable_fetch_ranges(obj: &object::File<'_>) -> Result<Vec<(u64, u64)>> {
    let mut out = Vec::new();
    let mut push = |addr: u64, len: usize| -> Result<()> {
        // Same overflow rule as `MemRegion::new`, so a range that could not be
        // built as a region is not silently accepted as one here.
        let end = addr.checked_add(len as u64).ok_or_else(|| {
            anyhow::anyhow!("region at {addr:#x} with length {len} would overflow u64")
        })?;
        out.push((addr, end));
        Ok(())
    };
    let (fetch, rom) = (LoadFilter::CodeAndReadOnly, LoadFilter::ImmutableOnly);
    match obj.kind() {
        ObjectKind::Executable | ObjectKind::Dynamic => {
            for seg in obj.segments() {
                let object::SegmentFlags::Elf { p_flags } = seg.flags() else {
                    continue;
                };
                if !fetch.segment_accepts(p_flags) || rom.segment_accepts(p_flags) {
                    continue;
                }
                let len = seg.data().context("failed to parse ELF")?.len();
                if len > 0 {
                    push(seg.address(), len)?;
                }
            }
        }
        _ => {
            // Section walk, matching `collect_loadable_sections_dedup`'s layout
            // and first-wins dedup so a losing section contributes no range.
            let layout = ElfSectionLayout::new(obj);
            let mut seen: std::collections::BTreeSet<u64> = std::collections::BTreeSet::new();
            for sec in obj.sections() {
                let object::read::SectionFlags::Elf { sh_flags } = sec.flags() else {
                    continue;
                };
                if !fetch.section_accepts(sh_flags) {
                    continue;
                }
                let len = sec.data().context("failed to parse ELF")?.len();
                if len == 0 {
                    continue;
                }
                let base = layout.section_base(&sec);
                if seen.insert(base) && !rom.section_accepts(sh_flags) {
                    push(base, len)?;
                }
            }
        }
    }
    Ok(out)
}
/// Where each section is actually loaded.
///
/// An ET_REL object is unlinked: `sh_addr` is 0 for every section, so `.text`,
/// `.text.startup` and `.rodata` all claim VMA 0. Assigning them distinct
/// addresses is the linker's job, and this does it: sections are walked in
/// index order and any whose `sh_addr` runs into already-placed space moves up
/// to `align_up(watermark, sh_addralign)`.
///
/// Populated only for an ET_REL. A linked image has real, disjoint `sh_addr`s,
/// so it holds no entries and every address passes through untouched.
/// The layout is filter-independent (every section participates, loaded or
/// not), so the fetch and ROM views agree on where a section is even when only
/// one of them maps it.
pub struct ElfSectionLayout {
    /// Section index -> loaded base. Absent means the section's own `sh_addr`.
    bases: BTreeMap<usize, u64>,
}

impl ElfSectionLayout {
    /// A section whose placement would run past `u64::MAX` keeps its `sh_addr`;
    /// building its [`MemRegion`] is what reports that overflow.
    pub fn new(obj: &object::File<'_>) -> Self {
        let mut bases = BTreeMap::new();
        // A linked image's section addresses are the real ones.
        if obj.kind() != ObjectKind::Relocatable {
            return Self { bases };
        }
        let mut watermark = 0u64;
        for sec in obj.sections() {
            let alloc = matches!(
                sec.flags(),
                object::read::SectionFlags::Elf { sh_flags }
                    if sh_flags & u64::from(object::elf::SHF_ALLOC) != 0
            );
            // `sh_size`, not the file bytes: SHT_NOBITS (`.bss`, `.tbss`) has
            // no bytes yet still occupies address space, and its symbols
            // resolve through this base.
            let size = sec.size();
            let addr = sec.address();
            // A non-allocatable or zero-size section occupies no address
            // space, so it stays at its `sh_addr`.
            let base = if !alloc || size == 0 || addr >= watermark {
                addr
            } else {
                align_up(watermark, sec.align())
            };
            if alloc && size != 0 {
                watermark = base.saturating_add(size);
            }
            bases.insert(sec.index().0, base);
        }
        Self { bases }
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
    pub fn symbol_address<'d>(&self, sym: &impl object::ObjectSymbol<'d>) -> u64 {
        let base = sym
            .section_index()
            .and_then(|i| self.base(i.0))
            .unwrap_or(0);
        base.wrapping_add(sym.address())
    }

    fn base(&self, section_index: usize) -> Option<u64> {
        self.bases.get(&section_index).copied()
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
/// Replays that walk rather than inferring it from the loaded bytes: which
/// sections survive depends on `filter`, and two sections holding equal bytes
/// are indistinguishable afterwards, so guessing would write `.rela.data`
/// straight over `.text.f`.
///
/// `filter` must be the one the regions were loaded with.
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
    filter: LoadFilter,
) -> Result<std::collections::BTreeSet<usize>> {
    if obj.kind() != ObjectKind::Relocatable {
        return Ok(std::collections::BTreeSet::new());
    }
    let layout = ElfSectionLayout::new(obj);
    let mut by_addr: BTreeMap<u64, usize> = BTreeMap::new();
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
        by_addr
            .entry(layout.section_base(&sec))
            .or_insert(sec.index().0);
    }
    Ok(by_addr.into_values().collect())
}

/// `bytes`, when given, is the whole image the regions are windows into; the
/// regions then share it instead of copying. `None` copies each mapping, for a
/// caller holding only a parsed [`object::File`].
///
/// # Errors
///
/// When an accepted segment or section's `data()` can't be read, or its
/// `address + length` would exceed `u64::MAX`.
pub(crate) fn collect_regions(
    obj: &object::File<'_>,
    bytes: Option<&FileBytes>,
    source: RegionSource,
    filter: LoadFilter,
) -> Result<Vec<MemRegion>> {
    match (source, obj.kind()) {
        (RegionSource::Auto, ObjectKind::Executable | ObjectKind::Dynamic) => {
            collect_loadable_segments(obj, bytes, filter)
        }
        // ET_REL plus any unknown / core kind. An `.o` has no program headers,
        // and a core dump's segment layout isn't what the analyser wants
        // either, so the section walk is the safer fallback for both.
        _ => collect_loadable_sections_dedup(obj, bytes, filter),
    }
}

/// One mapping of `data`, as a window into `bytes` when the image is at hand
/// and `range` is its file-backed extent, else as an owned copy.
fn region_from(
    bytes: Option<&FileBytes>,
    addr: u64,
    range: Option<(u64, u64)>,
    data: &[u8],
) -> Result<MemRegion> {
    match (bytes, range) {
        // The length gate rejects anything `data()` transformed (a compressed
        // section), where the file extent is not the mapping.
        (Some(bytes), Some((offset, len))) if len == data.len() as u64 => {
            MemRegion::window(addr, bytes, offset, len)
        }
        _ => MemRegion::new(addr, data.to_vec()),
    }
}

/// One [`MemRegion`] per accepted PT_LOAD segment, from its file-backed bytes.
/// Empty `data()` (a BSS-only segment, `p_filesz == 0`) has nothing to load and
/// is skipped.
fn collect_loadable_segments(
    obj: &object::File<'_>,
    bytes: Option<&FileBytes>,
    filter: LoadFilter,
) -> Result<Vec<MemRegion>> {
    let mut out = Vec::new();
    for seg in obj.segments() {
        // `obj.segments()` already yields PT_LOAD only, so `p_flags` is read
        // purely for the writable / executable filter axis.
        let object::SegmentFlags::Elf { p_flags } = seg.flags() else {
            continue;
        };
        if !filter.segment_accepts(p_flags) {
            continue;
        }
        let data = seg.data().context("failed to parse ELF")?;
        if data.is_empty() {
            continue;
        }
        out.push(region_from(
            bytes,
            seg.address(),
            Some(seg.file_range()),
            data,
        )?);
    }
    Ok(out)
}

/// One [`MemRegion`] per accepted file-backed section, at its
/// [`ElfSectionLayout`] base, under **first-wins dedup** on that base.
fn collect_loadable_sections_dedup(
    obj: &object::File<'_>,
    bytes: Option<&FileBytes>,
    filter: LoadFilter,
) -> Result<Vec<MemRegion>> {
    let layout = ElfSectionLayout::new(obj);
    let mut by_addr: BTreeMap<u64, MemRegion> = BTreeMap::new();
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
        let base = layout.section_base(&sec);
        if let std::collections::btree_map::Entry::Vacant(e) = by_addr.entry(base) {
            e.insert(region_from(bytes, base, sec.file_range(), data)?);
        }
    }
    Ok(by_addr.into_values().collect())
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
}
