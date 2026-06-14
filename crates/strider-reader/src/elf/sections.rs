//! ELF → [`MemRegion`] loaders.
//!
//! Two public entry points, both *kind-dispatched* on `obj.kind()`:
//!
//! - [`elf_get_loadable_regions`] — the code+read-only filter used by
//!   [`super::reader::ElfFileMemReader`].  An executable instruction or a
//!   compile-time-constant load may legitimately reference anything in
//!   this set (`.text`, `.rodata`, `.plt`, `.eh_frame`).
//! - [`elf_get_loadable_regions_including_writable`] — the broader
//!   include-writable filter used by
//!   [`super::relocations::apply_elf_relocations_autoload`].  Strictly a
//!   superset of [`elf_get_loadable_regions`]; also picks up
//!   `.data.rel.ro` / `.got.plt` / `.data` so relocations targeting
//!   writable runtime data have something to patch.
//!
//! # Dispatch
//!
//! For `ObjectKind::Executable` (ET_EXEC) and `ObjectKind::Dynamic`
//! (ET_DYN) the loader walks **program headers** (`obj.segments()` —
//! PT_LOAD entries).  Program headers are the canonical runtime memory
//! layout: a linked executable is described to the OS loader by its
//! program headers, not its section headers.  Section headers can be
//! stripped from a linked binary entirely and the OS will still load it
//! correctly; the section view is for debuggers / analysers.
//!
//! For `ObjectKind::Relocatable` (ET_REL — an `.o` object file) and any
//! other kind, the loader falls back to walking **sections** because
//! relocatable objects have *no* program headers at all (PT_LOAD only
//! appears post-link).  A relocatable section's `sh_addr` is typically 0
//! pre-link, so several sections (`.text`, `.text.startup`,
//! `.text.foo`, …) commonly share VMA 0.  To avoid the later section
//! silently shadowing the earlier one's bytes, ET_REL section
//! collection uses **first-wins** VMA dedup: the first section reaching
//! a given VMA keeps the slot; subsequent sections at the same VMA are
//! dropped.  For ET_REL `MemRegionsLookupTable`'s own
//! last-insert-wins rule would otherwise replace the earlier section's
//! bytes with whatever section happened to come later in iteration
//! order, which is non-deterministic from the user's perspective.

use std::collections::BTreeMap;

use anyhow::Context as _;
use object::{Object, ObjectKind, ObjectSection, ObjectSegment};

use crate::{MemRegion, Result};

/// What runtime-relevant bytes the loader should accept.
#[derive(Clone, Copy)]
enum LoadFilter {
    /// Executable + non-writable allocatable bytes only (`.text`,
    /// `.rodata`, `.plt`, `.eh_frame`).  Used by
    /// [`super::reader::ElfFileMemReader`] for instruction fetch and
    /// compile-time-constant loads.
    CodeAndReadOnly,
    /// Every allocatable file-backed mapping, including writable
    /// (`.data`, `.got`, `.data.rel.ro`).  Used by
    /// [`super::relocations::apply_elf_relocations_autoload`] so
    /// relocations targeting writable runtime data have something to
    /// patch.
    AllAllocatable,
}

impl LoadFilter {
    /// Does this filter accept a PT_LOAD segment with the given
    /// `p_flags`?  PT_LOAD is the only segment type the loader maps; the
    /// caller has already restricted iteration to PT_LOAD.
    ///
    /// `PF_W` indicates a writable mapping.  `CodeAndReadOnly` rejects
    /// writable mappings; `AllAllocatable` accepts every PT_LOAD.
    fn segment_accepts(self, p_flags: u32) -> bool {
        let is_writable = p_flags & object::elf::PF_W != 0;
        match self {
            LoadFilter::CodeAndReadOnly => !is_writable,
            LoadFilter::AllAllocatable => true,
        }
    }

    /// Does this filter accept a section with the given `sh_flags`?
    /// Sections always require `SHF_ALLOC`; the `SHF_WRITE` /
    /// `SHF_EXECINSTR` axes pick exec-or-rodata vs include-writable.
    fn section_accepts(self, sh_flags: u64) -> bool {
        let is_alloc = sh_flags & u64::from(object::elf::SHF_ALLOC) != 0;
        if !is_alloc {
            return false;
        }
        let is_exec = sh_flags & u64::from(object::elf::SHF_EXECINSTR) != 0;
        let is_writable = sh_flags & u64::from(object::elf::SHF_WRITE) != 0;
        match self {
            LoadFilter::CodeAndReadOnly => is_exec || !is_writable,
            LoadFilter::AllAllocatable => true,
        }
    }
}

/// Returns every code + read-only mapping the loader knows how to
/// surface for `obj`.
///
/// Dispatches on `obj.kind()`:
///
/// - **`Executable` / `Dynamic`** (ET_EXEC / ET_DYN): walks PT_LOAD
///   program headers (the runtime memory layout).  Writable segments
///   are excluded.
/// - **`Relocatable`** (ET_REL — `.o` object files), and any other
///   kind: walks sections, picking executable or non-writable
///   `SHF_ALLOC` sections.  First-wins VMA dedup applies — see the
///   module-level docs.
///
/// # Errors
///
/// Returns an `object::Error` if an accepted segment / section's
/// `data()` can't be read, or a `RegionOverflow` if any accepted
/// mapping's `address + length` would exceed `u64::MAX`.
pub fn elf_get_loadable_regions(obj: &object::File<'_>) -> Result<Vec<MemRegion>> {
    collect_loadable_regions(obj, LoadFilter::CodeAndReadOnly)
}

/// Like [`elf_get_loadable_regions`] but additionally includes
/// writable mappings (`.data.rel.ro`, `.got.plt`, `.data`).  Strictly
/// a superset.
///
/// Used by [`super::relocations::apply_elf_relocations_autoload`] so
/// dynamic relocations targeting writable runtime data have somewhere
/// to patch.  Same dispatch as [`elf_get_loadable_regions`] —
/// program-headers for ET_EXEC / ET_DYN, sections (with first-wins
/// VMA dedup) for ET_REL.
///
/// # Errors
///
/// Same as [`elf_get_loadable_regions`].
pub fn elf_get_loadable_regions_including_writable(
    obj: &object::File<'_>,
) -> Result<Vec<MemRegion>> {
    collect_loadable_regions(obj, LoadFilter::AllAllocatable)
}

/// Kind-dispatch: program-headers path for ET_EXEC / ET_DYN, sections
/// path (with first-wins VMA dedup) for ET_REL and everything else.
fn collect_loadable_regions(obj: &object::File<'_>, filter: LoadFilter) -> Result<Vec<MemRegion>> {
    match obj.kind() {
        ObjectKind::Executable | ObjectKind::Dynamic => collect_loadable_segments(obj, filter),
        // ET_REL (Relocatable) plus any unknown/core kind: the safest
        // fallback is the section-walker — an `.o` has no program
        // headers, and core dumps' segment layout isn't what the
        // analyser wants either.  Section-walker collects allocatable
        // sections under first-wins VMA dedup.
        _ => collect_loadable_sections_dedup(obj, filter),
    }
}

/// Walks `obj.segments()`, keeping every PT_LOAD entry whose `p_flags`
/// passes `filter`, and returns one [`MemRegion`] per accepted segment
/// (using the file-backed bytes — `segment.data()`).
///
/// Empty `data()` (a BSS-only segment with `p_filesz == 0`) is silently
/// skipped — there's nothing to load.
fn collect_loadable_segments(obj: &object::File<'_>, filter: LoadFilter) -> Result<Vec<MemRegion>> {
    let mut out = Vec::new();
    for seg in obj.segments() {
        // Restrict to PT_LOAD; `obj.segments()` already filters to
        // PT_LOAD on every backend that defines a meaningful "loadable"
        // segment, but we read `p_flags` here so check explicitly.
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
        out.push(MemRegion::new(seg.address(), data.to_vec())?);
    }
    Ok(out)
}

/// Walks `obj.sections()`, keeping every section whose `sh_flags`
/// passes `filter` and has file-backed bytes, and returns one
/// [`MemRegion`] per accepted section.
///
/// Uses **first-wins VMA dedup**: when two sections share the same
/// `sh_addr`, the first one encountered in iteration order keeps the
/// slot.  `.o` (ET_REL) files commonly have several sections at
/// VMA 0 pre-link (`.text`, `.text.startup`, …); without this dedup
/// `MemRegionsLookupTable`'s own last-insert-wins rule would
/// non-deterministically swap one section's bytes for another's.
fn collect_loadable_sections_dedup(
    obj: &object::File<'_>,
    filter: LoadFilter,
) -> Result<Vec<MemRegion>> {
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
        // First-wins: only build a `MemRegion` (which copies `data`) when
        // no region already occupies this start address.  Avoids the
        // copy on the loser of a VMA collision (`.text` vs `.text.foo`
        // in an ET_REL `.o` before linking).
        if let std::collections::btree_map::Entry::Vacant(e) = by_addr.entry(sec.address()) {
            let region = MemRegion::new(sec.address(), data.to_vec())?;
            e.insert(region);
        }
    }
    Ok(by_addr.into_values().collect())
}
