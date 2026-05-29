//! ELF → [`MemRegion`] section walkers.
//!
//! Two presets:
//! - [`elf_get_code_and_readonly_sections_as_mem_regions`] — `SHF_ALLOC &&
//!   (SHF_EXECINSTR || !SHF_WRITE)`; the preset used by
//!   [`super::reader::ElfFileMemReader`].
//! - [`elf_get_allocatable_file_backed_sections_as_mem_regions`] — every
//!   `SHF_ALLOC` section with file-backed bytes; the wider preset used by
//!   [`super::relocations::apply_elf_relocations_autoload`] so dynamic relocs
//!   targeting writable sections (`.got.plt`, `.data.rel.ro`, …) have
//!   something to patch.

use anyhow::Context as _;
use object::{Object, ObjectSection};

use crate::{MemRegion, Result};

/// Walks every section of `obj` and returns a [`MemRegion`] for each one
/// that matches `filter` and has file-backed data.  Centralises the
/// `section.data() + MemRegion::new` plumbing the two surviving presets
/// share so neither has to repeat the empty-data skip + overflow handling.
///
/// `filter`-rejected sections are never read, so a malformed rejected
/// section cannot spuriously surface as a parse error.  Accepted sections
/// whose `data()` returns empty bytes (e.g. `SHT_NOBITS` like `.bss`) are
/// silently skipped — there's nothing to load.  Iteration order is
/// preserved; duplicate `start_addr`s are resolved later by
/// [`crate::MemRegionsLookupTable`] under its "last insert wins" rule.
fn collect_sections_as_mem_regions(
    obj: &object::File<'_>,
    filter: impl Fn(&object::read::Section<'_, '_>) -> bool,
) -> Result<Vec<MemRegion>> {
    let mut out = Vec::new();
    for sec in obj.sections() {
        if !filter(&sec) {
            continue;
        }
        let data = sec.data().context("failed to parse ELF")?;
        if data.is_empty() {
            continue;
        }
        out.push(MemRegion::new(sec.address(), data.to_vec())?);
    }
    Ok(out)
}

fn section_is_exec_or_readonly(sec: &object::read::Section<'_, '_>) -> bool {
    let object::read::SectionFlags::Elf { sh_flags } = sec.flags() else {
        return false;
    };
    let is_alloc    = sh_flags & u64::from(object::elf::SHF_ALLOC)     != 0;
    let is_exec     = sh_flags & u64::from(object::elf::SHF_EXECINSTR) != 0;
    let is_writable = sh_flags & u64::from(object::elf::SHF_WRITE)     != 0;
    is_alloc && (is_exec || !is_writable)
}

/// Returns all sections an executed instruction or a compile-time-constant
/// load could legitimately reference: anything with file-backed data that is
/// either executable or not writable.
///
/// This includes `.text` (exec), `.rodata` (non-writable data), `.plt`,
/// `.eh_frame`, etc. It excludes `.data`, `.bss`, and any other writable or
/// `SHT_NOBITS` section. Only sections with `SHF_ALLOC` are included — a
/// non-loadable section like `.shstrtab` has no runtime address and is not
/// valid for a memory reader even if it happens to be non-writable.
///
/// # Errors
///
/// Returns an `object::Error` if an accepted section's `data()` can't be
/// read, or a `RegionOverflow` if `address() + data.len()` would exceed
/// `u64::MAX`.
pub fn elf_get_code_and_readonly_sections_as_mem_regions(
    obj: &object::File<'_>,
) -> Result<Vec<MemRegion>> {
    collect_sections_as_mem_regions(obj, section_is_exec_or_readonly)
}

/// Returns every allocatable file-backed section as a [`MemRegion`].
/// Strictly broader than [`elf_get_code_and_readonly_sections_as_mem_regions`]:
/// includes writable sections like `.data.rel.ro`, `.data`, `.got`, …
/// — anywhere the linker emitted runtime-relocated data the analyser
/// might want to read.
///
/// Used by callers that intend to apply ELF relocations
/// post-load: function-pointer tables in `.data.rel.ro` and
/// indirect-call targets in `.got` need to be loaded so the
/// applier has somewhere to patch.  Without this widening, an
/// `R_*_RELATIVE` relocation against `.data.rel.ro` falls through
/// `apply_elf_relocations`'s "no region" skip path and the table
/// reads zero at analysis time.
///
/// `SHT_NOBITS` (`.bss`, `.tbss`) sections produce empty `data()`
/// and are skipped — there's nothing to patch and the analyser has
/// no need for zero-filled regions.
///
/// # Errors
///
/// Returns an `object::Error` if an accepted section's `data()` can't be
/// read, or a `RegionOverflow` if `address() + data.len()` would exceed
/// `u64::MAX`.
pub fn elf_get_allocatable_file_backed_sections_as_mem_regions(
    obj: &object::File<'_>,
) -> Result<Vec<MemRegion>> {
    collect_sections_as_mem_regions(obj, |sec| {
        let object::read::SectionFlags::Elf { sh_flags } = sec.flags() else {
            return false;
        };
        sh_flags & u64::from(object::elf::SHF_ALLOC) != 0
    })
}
