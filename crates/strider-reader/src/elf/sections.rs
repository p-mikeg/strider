//! ELF to [`MemRegion`] loaders.
//!
//! # Dispatch
//!
//! ET_EXEC / ET_DYN walk **program headers** (PT_LOAD). Those are the canonical
//! runtime layout: the OS loads a linked binary from its program headers, and
//! section headers can be stripped entirely without affecting that. The section
//! view exists for debuggers and analysers.
//!
//! ET_REL and every other kind fall back to walking **sections**, since a
//! relocatable object has no program headers at all (PT_LOAD only appears
//! post-link). Pre-link `sh_addr` is typically 0, so `.text`, `.text.startup`,
//! `.text.foo` commonly share VMA 0. Section collection therefore uses
//! **first-wins** VMA dedup: without it, `MemRegionsLookupTable`'s
//! last-insert-wins rule would pick whichever section came later in iteration
//! order, which is non-deterministic from the user's perspective.

use std::collections::BTreeMap;

use anyhow::Context as _;
use object::{Object, ObjectKind, ObjectSection, ObjectSegment};

use crate::{MemRegion, Result};

#[derive(Clone, Copy)]
enum LoadFilter {
    /// `.text`, `.rodata`, `.plt`, `.eh_frame`: what an instruction fetch or a
    /// constant-address load may legitimately reference.
    CodeAndReadOnly,
    /// Also `.data`, `.got`, `.data.rel.ro`, so relocations targeting writable
    /// runtime data have something to patch.
    AllAllocatable,
}

impl LoadFilter {
    /// Callers have already restricted iteration to PT_LOAD, so only the `PF_W`
    /// axis matters here.
    fn segment_accepts(self, p_flags: u32) -> bool {
        let is_writable = p_flags & object::elf::PF_W != 0;
        match self {
            LoadFilter::CodeAndReadOnly => !is_writable,
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
            LoadFilter::AllAllocatable => true,
        }
    }
}

/// Every code + read-only mapping, kind-dispatched per the module docs:
/// PT_LOAD program headers for ET_EXEC / ET_DYN (writable segments excluded),
/// sections with first-wins VMA dedup otherwise.
///
/// # Errors
///
/// When an accepted segment or section's `data()` can't be read, or its
/// `address + length` would exceed `u64::MAX`.
pub fn elf_get_loadable_regions(obj: &object::File<'_>) -> Result<Vec<MemRegion>> {
    collect_loadable_regions(obj, LoadFilter::CodeAndReadOnly)
}

/// A strict superset of [`elf_get_loadable_regions`], adding writable mappings
/// (`.data.rel.ro`, `.got.plt`, `.data`) so dynamic relocations targeting
/// writable runtime data have somewhere to patch. Same dispatch.
///
/// # Errors
///
/// Same as [`elf_get_loadable_regions`].
pub fn elf_get_loadable_regions_including_writable(
    obj: &object::File<'_>,
) -> Result<Vec<MemRegion>> {
    collect_loadable_regions(obj, LoadFilter::AllAllocatable)
}

/// Forces the section walk (with first-wins VMA dedup) regardless of
/// `obj.kind()`, even for an ET_EXEC / ET_DYN binary that does carry PT_LOAD
/// segments. For callers wanting section-granular regions (`.text` / `.rodata`
/// / `.plt` as separate mappings) instead of coalesced PT_LOAD ranges.
///
/// # Errors
///
/// Same as [`elf_get_loadable_regions`].
pub fn elf_get_loadable_regions_sections_only(obj: &object::File<'_>) -> Result<Vec<MemRegion>> {
    collect_loadable_sections_dedup(obj, LoadFilter::CodeAndReadOnly)
}

/// [`elf_get_loadable_regions_sections_only`] plus the writable mappings.
///
/// # Errors
///
/// Same as [`elf_get_loadable_regions`].
pub fn elf_get_loadable_regions_sections_only_including_writable(
    obj: &object::File<'_>,
) -> Result<Vec<MemRegion>> {
    collect_loadable_sections_dedup(obj, LoadFilter::AllAllocatable)
}

fn collect_loadable_regions(obj: &object::File<'_>, filter: LoadFilter) -> Result<Vec<MemRegion>> {
    match obj.kind() {
        ObjectKind::Executable | ObjectKind::Dynamic => collect_loadable_segments(obj, filter),
        // ET_REL plus any unknown / core kind. An `.o` has no program headers,
        // and a core dump's segment layout isn't what the analyser wants
        // either, so the section walk is the safer fallback for both.
        _ => collect_loadable_sections_dedup(obj, filter),
    }
}

/// One [`MemRegion`] per accepted PT_LOAD segment, from its file-backed bytes.
/// Empty `data()` (a BSS-only segment, `p_filesz == 0`) has nothing to load and
/// is skipped.
fn collect_loadable_segments(obj: &object::File<'_>, filter: LoadFilter) -> Result<Vec<MemRegion>> {
    let mut out = Vec::new();
    for seg in obj.segments() {
        // `obj.segments()` already yields PT_LOAD only (the `object` crate
        // filters on `p_type` internally, and the generic `Segment` trait
        // exposes no `p_type` to re-assert). `p_flags` is read purely for the
        // writable / executable filter axis, not to re-check segment type.
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

/// One [`MemRegion`] per accepted file-backed section, under **first-wins VMA
/// dedup**: when two sections share an `sh_addr`, the first encountered keeps
/// the slot. See the module docs for why last-wins is not acceptable here.
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
        // Build the `MemRegion` (which copies `data`) only on a vacant entry,
        // so the loser of a VMA collision costs no copy.
        if let std::collections::btree_map::Entry::Vacant(e) = by_addr.entry(sec.address()) {
            let region = MemRegion::new(sec.address(), data.to_vec())?;
            e.insert(region);
        }
    }
    Ok(by_addr.into_values().collect())
}
