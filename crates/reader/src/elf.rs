//! ELF-backed implementation of [`crate::MemRegion`]s and the
//! [`rsleigh::MemReader`] trait.
//!
//! This module is the ELF-specific half of the `reader` crate. The generic
//! region-lookup machinery (`MemRegion`, `MemRegionsLookupTable`) lives in
//! [`crate`] so other backends (raw blobs, PE, Mach-O, …) can reuse it.
//!
//! # Converter API
//!
//! Single-item converters (take one segment/section, return one region):
//! - [`elf_segment_to_mem_region`]
//! - [`elf_section_to_mem_region`]
//!
//! Batch converters (iterate all segments/sections, return matching regions):
//! - [`elf_segments_to_mem_regions`] (filter by any predicate)
//! - [`elf_sections_to_mem_regions`] (filter by any predicate)
//!
//! Filter presets (batch converters wired to common predicates):
//! - [`elf_get_executable_segments_as_mem_regions`] — `PF_X`
//! - [`elf_get_executable_sections_as_mem_regions`] — `SHF_EXECINSTR`
//! - [`elf_get_code_and_readonly_sections_as_mem_regions`] — `SHF_ALLOC &&
//!   (SHF_EXECINSTR || !SHF_WRITE)`; the preset used by [`ElfFileMemReader`].
//!
//! Top-level helpers:
//! - [`ElfFileMemReader`] — owns its regions; implements both
//!   [`rsleigh::MemReader`] and [`crate::ReadOnlyMemory`].
//! - [`load_elf`] — reads an ELF from disk and returns a `'static`-lifetime
//!   parsed `object::File` (intentionally leaks the backing bytes).

use anyhow::Context as _;
use object::{
    Object, ObjectSection, ObjectSegment, ObjectSymbol, ObjectSymbolTable,
    RelocationFlags, RelocationKind, RelocationTarget,
};

use crate::{MemRegion, MemRegionsLookupTable, Result};

// ── ELF → MemRegion converters ────────────────────────────────────────────────

/// Converts a single ELF segment into a [`MemRegion`].
///
/// # Errors
///
/// Returns an error when the segment's file-backed data cannot be
/// read, or when `segment.address() + data.len()` would exceed
/// `u64::MAX`.
pub fn elf_segment_to_mem_region(segment: &object::read::Segment<'_, '_>) -> Result<MemRegion> {
    let data = segment.data().context("failed to parse ELF")?;
    MemRegion::new(segment.address(), data.to_vec())
}

/// Converts a single ELF section into a [`MemRegion`].
///
/// # Errors
///
/// Returns an error when the section's file-backed data cannot be
/// read, or when `section.address() + data.len()` would exceed
/// `u64::MAX`.
pub fn elf_section_to_mem_region(section: &object::read::Section<'_, '_>) -> Result<MemRegion> {
    let data = section.data().context("failed to parse ELF")?;
    MemRegion::new(section.address(), data.to_vec())
}

/// Collects ELF segments into [`MemRegion`]s, keeping only those for which
/// `filter` returns `true`.
///
/// Segments with empty data (e.g. `PT_LOAD` with `p_filesz == 0`, where
/// `data()` returns `Ok(&[])`) are skipped. Preserves iteration order;
/// duplicate `start_addr`s are resolved later by [`MemRegionsLookupTable`]
/// under its "last one inserted wins" rule.
///
/// # Errors
///
/// Returns an error when a segment accepted by `filter` has
/// file-backed data that cannot be read (segments rejected by
/// `filter` are never read, so malformed rejected segments do not
/// surface), or when an accepted segment's `address() + data.len()`
/// would exceed `u64::MAX`.
///
/// Accepted empty-data segments (e.g. `p_filesz == 0`) are skipped rather
/// than reported.
pub fn elf_segments_to_mem_regions(
    obj: &object::File<'_>,
    filter: impl Fn(&object::read::Segment<'_, '_>) -> bool,
) -> Result<Vec<MemRegion>> {
    let mut out = Vec::new();
    for seg in obj.segments() {
        if !filter(&seg) {
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

/// Collects ELF sections into [`MemRegion`]s, keeping only those for which
/// `filter` returns `true`.
///
/// Sections whose `data()` returns empty bytes (e.g. `SHT_NOBITS` like
/// `.bss`) are skipped. Preserves iteration order; duplicate `start_addr`s
/// are resolved later by [`MemRegionsLookupTable`] under its "last one
/// inserted wins" rule.
///
/// # Errors
///
/// Returns an error when a section accepted by `filter` has
/// file-backed data that cannot be read (sections rejected by
/// `filter` are never read, so malformed rejected sections do not
/// surface), or when an accepted section's `address() + data.len()`
/// would exceed `u64::MAX`.
///
/// Accepted empty-data sections (e.g. `SHT_NOBITS`-equivalents) are
/// skipped rather than reported.
pub fn elf_sections_to_mem_regions(
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

// ── Executable-only helpers ───────────────────────────────────────────────────

fn segment_is_executable(seg: &object::read::Segment<'_, '_>) -> bool {
    matches!(
        seg.flags(),
        object::read::SegmentFlags::Elf { p_flags }
            if p_flags & object::elf::PF_X != 0
    )
}

fn section_is_executable(sec: &object::read::Section<'_, '_>) -> bool {
    matches!(
        sec.flags(),
        object::read::SectionFlags::Elf { sh_flags }
            if sh_flags & u64::from(object::elf::SHF_EXECINSTR) != 0
    )
}

fn section_is_code_or_readonly(sec: &object::read::Section<'_, '_>) -> bool {
    let object::read::SectionFlags::Elf { sh_flags } = sec.flags() else {
        return false;
    };
    let is_alloc    = sh_flags & u64::from(object::elf::SHF_ALLOC)     != 0;
    let is_exec     = sh_flags & u64::from(object::elf::SHF_EXECINSTR) != 0;
    let is_writable = sh_flags & u64::from(object::elf::SHF_WRITE)     != 0;
    is_alloc && (is_exec || !is_writable)
}

/// Returns all executable ELF segments (i.e. those with the `PF_X` flag set)
/// as [`MemRegion`]s.
///
/// # Errors
///
/// Propagates any error from the underlying segment iteration; see
/// [`elf_segments_to_mem_regions`] for the full error set
/// (`Object` + `RegionOverflow`).
pub fn elf_get_executable_segments_as_mem_regions(
    obj: &object::File<'_>,
) -> Result<Vec<MemRegion>> {
    elf_segments_to_mem_regions(obj, segment_is_executable)
}

/// Returns all executable ELF sections (i.e. those with `SHF_EXECINSTR` set)
/// as [`MemRegion`]s.
///
/// # Errors
///
/// Propagates any error from the underlying section iteration; see
/// [`elf_sections_to_mem_regions`] for the full error set
/// (`Object` + `RegionOverflow`).
pub fn elf_get_executable_sections_as_mem_regions(
    obj: &object::File<'_>,
) -> Result<Vec<MemRegion>> {
    elf_sections_to_mem_regions(obj, section_is_executable)
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
/// Propagates any error from the underlying section iteration; see
/// [`elf_sections_to_mem_regions`] for the full error set
/// (`Object` + `RegionOverflow`).
pub fn elf_get_code_and_readonly_sections_as_mem_regions(
    obj: &object::File<'_>,
) -> Result<Vec<MemRegion>> {
    elf_sections_to_mem_regions(obj, section_is_code_or_readonly)
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
/// Propagates any error from [`elf_sections_to_mem_regions`].
pub fn elf_get_allocatable_file_backed_sections_as_mem_regions(
    obj: &object::File<'_>,
) -> Result<Vec<MemRegion>> {
    elf_sections_to_mem_regions(obj, |sec| {
        let object::read::SectionFlags::Elf { sh_flags } = sec.flags() else {
            return false;
        };
        sh_flags & u64::from(object::elf::SHF_ALLOC) != 0
    })
}

// ── ElfFileMemReader ──────────────────────────────────────────────────────────

/// An rsleigh [`rsleigh::MemReader`] backed by an ELF file's sections.
///
/// The reader owns its backing bytes (copied into [`MemRegion`]s at
/// construction) so no lifetime borrow on the source `object::File` or its
/// byte buffer is required. Both the executable sections (for instruction
/// fetch) and the read-only data sections (for compile-time-constant loads)
/// are loaded from the same ELF.
#[derive(Debug)]
pub struct ElfFileMemReader {
    lookup: MemRegionsLookupTable,
    is_little_endian: bool,
}

impl ElfFileMemReader {
    /// Builds a reader from an already-parsed [`object::File`].
    ///
    /// Loads every executable section and every non-writable section with
    /// file-backed data. The parsed object is not retained — the returned
    /// reader is self-owning.
    ///
    /// # Errors
    ///
    /// Propagates any error from
    /// [`elf_get_code_and_readonly_sections_as_mem_regions`]: `Object` for
    /// unreadable section data and `RegionOverflow` if any included
    /// section's `address() + data.len()` would exceed `u64::MAX`.
    pub fn from_object(obj: &object::File<'_>) -> Result<Self> {
        let regions = elf_get_code_and_readonly_sections_as_mem_regions(obj)?;
        Ok(Self {
            lookup: MemRegionsLookupTable::new(regions),
            is_little_endian: matches!(obj.endianness(), object::Endianness::Little),
        })
    }

    /// Builds a reader by parsing the given ELF bytes.
    ///
    /// The bytes are parsed in-place; no leak is required.
    ///
    /// # Errors
    ///
    /// Returns an error if the bytes fail to parse as a valid ELF,
    /// or any error produced by [`from_object`](Self::from_object).
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        let obj = object::File::parse(bytes).context("failed to parse ELF")?;
        Self::from_object(&obj)
    }

    /// Builds a reader by reading and parsing an ELF file from disk.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be read from disk, or
    /// any error produced by [`from_bytes`](Self::from_bytes).
    pub fn from_path<P: AsRef<std::path::Path>>(path: P) -> Result<Self> {
        let bytes = std::fs::read(path).context("failed to read file")?;
        Self::from_bytes(&bytes)
    }
}

impl rsleigh::MemReader for ElfFileMemReader {
    type Err = anyhow::Error;

    fn read(&self, addr: rsleigh::VnAddr, out_buf: &mut [u8]) -> Result<usize> {
        self.lookup
            .read(addr.off, out_buf)
            .ok_or_else(|| anyhow::anyhow!("address {:#x} is not mapped", addr.off))
    }
}

impl crate::ReadOnlyMemory for ElfFileMemReader {
    fn read(&self, space: rsleigh::VnSpace, addr: u64, size: usize) -> Option<u64> {
        if space != rsleigh::VnSpace::RAM {
            return None;
        }
        if size == 0 || size > 8 {
            return None;
        }
        // Place the read bytes at the endianness-appropriate end of an 8-byte
        // buffer so the final from_{le,be}_bytes produces the same numeric
        // value for an N-byte load as the target machine would.
        let is_little = self.is_little_endian;
        let mut buf = [0u8; 8];
        let slot = if is_little {
            &mut buf[..size]
        } else {
            &mut buf[8 - size..]
        };
        if self.lookup.read(addr, slot)? != size {
            return None;
        }
        Some(if is_little {
            u64::from_le_bytes(buf)
        } else {
            u64::from_be_bytes(buf)
        })
    }
}

// ── ELF relocation application ───────────────────────────────────────────────
//
// FreeBSD kernels and other ET_DYN binaries ship with unresolved
// relocations: a `call rel32` to a function in the same image is
// stored as `e8 00 00 00 00` until the loader patches the 4-byte
// immediate with `target - (rip)`.  Without this patch the analyser
// follows the call as control flow into the next instruction (rel32
// = 0 → call site + 5 bytes) and prunes any code that only fed the
// real call target.  The loader does this work at runtime; for
// static analysis we replicate it here, in-place on the loaded
// `MemRegion`s.
//
// Architecture-independence is delegated to the `object` crate's
// `RelocationKind` enum (`Absolute` = `S + A`, `Relative` = `S + A
// - P`, `PltRelative` = `L + A - P` — modelled as `Relative` here
// because we don't materialise PLT stubs).  Anything else we
// recognise but skip on; unknown architectures are skipped silently
// rather than producing partial / mis-applied patches.

/// Result counts from [`apply_elf_relocations`].  Returned as a
/// breakdown rather than a single integer so callers can surface
/// "this binary had 1234 relocations, we applied 1000 of them" to
/// the user when something looks off.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RelocationStats {
    /// Total relocations seen across every iterated table.
    pub seen: usize,
    /// Relocations the applier patched into the loaded regions.
    pub applied: usize,
    /// Relocations skipped because the target symbol could not be
    /// resolved (typically: undefined externs, weak symbols
    /// referencing absent libraries).  Patching with an arbitrary
    /// value would produce wrong control flow.
    pub skipped_unresolved_target: usize,
    /// Relocations skipped because their kind / size / encoding
    /// isn't one the applier knows how to write — e.g. ARM Thumb-
    /// branch encodings or platform-specific TLS variants.
    pub skipped_unsupported_kind: usize,
    /// Relocations whose site address didn't fall inside any of the
    /// `regions` passed in (e.g. .data / .bss relocations when the
    /// caller only loaded code-and-readonly sections).
    pub skipped_no_region: usize,
    /// Sorted+deduped list of raw ELF `r_type` codes the applier
    /// classified as unsupported (i.e. that incremented
    /// `skipped_unsupported_kind`).  Surfaces *which* kinds the
    /// binary actually uses that we don't model; pair with
    /// `obj.architecture()` and the System V ABI per-arch
    /// relocation tables to identify each.  Empty when
    /// `skipped_unsupported_kind == 0`.
    pub unsupported_r_types: Vec<u32>,
}

/// Patches relocations in `regions` in-place using the ELF's
/// dynamic relocation table.
///
/// Walks `obj.dynamic_relocations()` (Relocations from `.rela.dyn`,
/// `.rela.plt`, …).  For each entry, computes the target address
/// from the relocation's symbol or section, then writes the encoded
/// value into the region containing the relocation site.
///
/// Used for ET_DYN binaries (FreeBSD kernels, PIE userland) whose
/// `.text` ships with unresolved `call rel32` placeholders.  ET_REL
/// binaries (relocatable object files) keep their relocations on
/// per-section tables and are not currently iterated here — the
/// dynamic-relocations API is sufficient for the kernel + PIE
/// shapes that motivated this function.
///
/// # Supported relocation kinds
///
/// Per [`RelocationKind`] (object's high-level enum):
/// * `Absolute` — `S + A` (e.g. `R_X86_64_64`).  Symbol-targeted.
/// * `Relative` — `S + A - P` (e.g. `R_X86_64_PC32`,
///   `R_X86_64_PC64`, `R_AARCH64_PREL32/PREL64`,
///   `R_386_PC32`).  Symbol-targeted.
/// * `PltRelative` — same value as `Relative` (e.g.
///   `R_X86_64_PLT32`, `R_AARCH64_CALL26`, `R_386_PLT32`).  We
///   don't materialise a PLT — the symbol's own address is used.
///
/// Per raw `r_type` (object reports these as
/// `RelocationKind::Unknown`):
/// * `R_X86_64_RELATIVE` / `R_386_RELATIVE` /
///   `R_AARCH64_RELATIVE` / `R_ARM_RELATIVE` — write
///   `image_base + addend` (image_base modelled as 0).
/// * `R_X86_64_GLOB_DAT` / `R_X86_64_JUMP_SLOT` and the
///   i386 / aarch64 / arm equivalents — write the symbol's
///   address at the slot (S semantics, eagerly resolved).
///
/// Symbol indices in dynamic relocations reference the dynamic
/// symbol table (`.dynsym`); the applier dispatches through
/// `obj.dynamic_symbol_table()` and falls back to `.symtab` only
/// for ET_REL files where `dynamic_symbol_table()` is `None`.
///
/// # NOT supported
///
/// * `Got` / `GotRelative` / `GotBaseRelative` / `GotBaseOffset` —
///   the analyser would need a synthesised GOT section to write
///   into, which we don't allocate.
/// * Architecture-specific encodings whose payload doesn't fit a
///   simple `value_low_bytes_at_offset` write (Thumb branches,
///   AArch64 ADR_PREL_PG_HI21 + ADD_ABS_LO12_NC pairs, MIPS
///   HI16/LO16 splits, PPC TOC relocations).  These come back as
///   `RelocationKind::Unknown` with `size = 0` and are counted
///   under `skipped_unsupported_kind`.
/// * MIPS / PowerPC kernel/userland binaries' specialised
///   relocation types — none of strider's current users hit them,
///   so they're left as a future-work item.
///
/// # Errors
///
/// Returns an error only on a malformed ELF (a relocation whose
/// target symbol or section index doesn't resolve).  A bad symbol
/// resolution that arises legitimately (e.g. `STN_UNDEF` for an
/// external lib) is *not* an error — the relocation is simply
/// counted under `skipped_unresolved_target` and the caller can
/// log/inspect the result.
pub fn apply_elf_relocations(
    regions: &mut [MemRegion],
    obj: &object::File<'_>,
) -> Result<RelocationStats> {
    let endian_le = matches!(obj.endianness(), object::Endianness::Little);
    let mut stats = RelocationStats::default();

    let Some(dyn_relocs) = obj.dynamic_relocations() else {
        return Ok(stats);
    };

    for (site_addr, reloc) in dyn_relocs {
        stats.seen += 1;

        // Image-relative relocations (`R_X86_64_RELATIVE` /
        // `R_AARCH64_RELATIVE` / `R_386_RELATIVE` / `R_ARM_RELATIVE`)
        // store `image_base + addend` at the site, with no symbol or
        // section reference — `RelocationTarget::Absolute` and a
        // `RelocationKind::Unknown` come out the other side of object
        // crate's mapping table.  We model the analyser's image base
        // as the binary's link-time-chosen base (typically 0 for an
        // ET_DYN), so the patched value is `addend` directly.  Width
        // is fixed by the relocation type (64-bit on 64-bit ABIs,
        // 32-bit on 32-bit).  Without this branch every PIE binary's
        // `dispatch_table[]` slot reads zero post-load.
        if let Some((value, size_bytes)) = image_relative_reloc(&reloc, obj.architecture()) {
            if let Some(region) = regions
                .iter_mut()
                .find(|r| r.contains(site_addr) && site_addr + size_bytes as u64 <= r.end_addr())
            {
                let off = (site_addr - region.start_addr()) as usize;
                write_at(region.data_mut(), off, value, size_bytes, endian_le);
                stats.applied += 1;
            } else {
                stats.skipped_no_region += 1;
            }
            continue;
        }

        // GOT-data and PLT-jump slots (`R_*_GLOB_DAT` / `R_*_JUMP_SLOT`).
        // Object 0.38 reports these as `RelocationKind::Unknown` with
        // `size = 0`, but they have well-defined "S" semantics: write
        // the symbol's address at the site.  Resolving eagerly means
        // analysis-time `Load(GOT[...])` reads the real target without
        // having to model a PLT.
        if let Some(size_bytes) = got_or_plt_slot_reloc_size(&reloc, obj.architecture()) {
            // Need the target symbol for these (no symbol → skip).
            let target_addr = match reloc.target() {
                RelocationTarget::Symbol(idx) => {
                    let resolved = if let Some(dynsym) = obj.dynamic_symbol_table() {
                        dynsym
                            .symbol_by_index(idx)
                            .map(|s| (s.address(), s.is_undefined()))
                            .ok()
                    } else {
                        obj.symbol_by_index(idx)
                            .map(|s| (s.address(), s.is_undefined()))
                            .ok()
                    };
                    let Some((addr, undef)) = resolved else {
                        stats.skipped_unresolved_target += 1;
                        continue;
                    };
                    if addr == 0 && undef {
                        stats.skipped_unresolved_target += 1;
                        continue;
                    }
                    addr
                }
                _ => {
                    stats.skipped_unresolved_target += 1;
                    continue;
                }
            };
            let value = target_addr.wrapping_add(reloc.addend() as u64);
            if let Some(region) = regions
                .iter_mut()
                .find(|r| r.contains(site_addr) && site_addr + size_bytes as u64 <= r.end_addr())
            {
                let off = (site_addr - region.start_addr()) as usize;
                write_at(region.data_mut(), off, value, size_bytes, endian_le);
                stats.applied += 1;
            } else {
                stats.skipped_no_region += 1;
            }
            continue;
        }

        // Resolve the target.  Per `Object::dynamic_relocations`'s
        // doc-comment, symbol indices here reference the dynamic
        // symbol table — `obj.symbol_by_index` looks at `.symtab`
        // and returns the wrong entry for a given index, so we
        // must dispatch through `dynamic_symbol_table()` first and
        // fall back to the static `.symtab` only if the dynamic
        // table is absent (ET_REL files).
        let target_addr = match reloc.target() {
            RelocationTarget::Symbol(idx) => {
                let resolved = if let Some(dynsym) = obj.dynamic_symbol_table() {
                    dynsym
                        .symbol_by_index(idx)
                        .map(|s| (s.address(), s.is_undefined()))
                        .ok()
                } else {
                    obj.symbol_by_index(idx)
                        .map(|s| (s.address(), s.is_undefined()))
                        .ok()
                };
                let Some((addr, undef)) = resolved else {
                    stats.skipped_unresolved_target += 1;
                    continue;
                };
                if addr == 0 && undef {
                    stats.skipped_unresolved_target += 1;
                    continue;
                }
                addr
            }
            RelocationTarget::Section(idx) => match obj.section_by_index(idx) {
                Ok(sec) => sec.address(),
                Err(_) => {
                    stats.skipped_unresolved_target += 1;
                    continue;
                }
            },
            // `Absolute` (sentinel for immediate-value relocations
            // with no symbol/section) and any future variants get
            // bucketed as unsupported.
            _ => {
                record_unsupported(&reloc, &mut stats);
                continue;
            }
        };

        let addend = reloc.addend();
        // S, A, P naming follows the System V ABI generic relocation
        // formula (see object::common::RelocationKind doc-comment):
        //   S = target_addr, A = addend, P = site_addr.
        let value = match reloc.kind() {
            RelocationKind::Absolute => target_addr.wrapping_add(addend as u64),
            // L (PLT entry) is treated as the symbol's own address —
            // we don't materialise a PLT.  Functionally identical to
            // `Relative` for analysis purposes.
            RelocationKind::Relative | RelocationKind::PltRelative => target_addr
                .wrapping_add(addend as u64)
                .wrapping_sub(site_addr),
            _ => {
                record_unsupported(&reloc, &mut stats);
                continue;
            }
        };

        // The `size` field is in bits; `size == 0` means "use the
        // kind's default", but the only kinds we patch (Absolute /
        // Relative / PltRelative) all set `size` explicitly on every
        // arch we care about, so a 0 size signals an arch-specific
        // encoding (e.g. ARM Thumb branch) we don't model.
        let size_bits = reloc.size();
        if size_bits == 0 || size_bits % 8 != 0 || size_bits > 64 {
            stats.skipped_unsupported_kind += 1;
            continue;
        }
        let size_bytes = (size_bits / 8) as usize;

        // Find the region that contains the [site_addr, site_addr +
        // size_bytes) range.  Linear scan is fine here — relocation
        // counts are small relative to the per-relocation work.
        let Some(region) = regions
            .iter_mut()
            .find(|r| r.contains(site_addr) && site_addr + size_bytes as u64 <= r.end_addr())
        else {
            stats.skipped_no_region += 1;
            continue;
        };

        let off = (site_addr - region.start_addr()) as usize;
        write_at(region.data_mut(), off, value, size_bytes, endian_le);
        stats.applied += 1;
    }

    Ok(stats)
}

/// Like [`apply_elf_relocations`], but pre-walks the dynamic
/// relocation table and lazily extends `regions` with any
/// `SHF_ALLOC` file-backed section from `obj` that contains a
/// relocation site not yet covered by an existing region.  Then
/// delegates to the pure [`apply_elf_relocations`].
///
/// Use this when the caller has already loaded a curated subset
/// of the ELF (e.g. only code+rodata) but wants relocation
/// application to "just work" without needing to know upfront
/// which writable sections (`.got.plt`, `.data.rel.ro`, …) the
/// dynamic relocs target.  Avoids the silent-failure mode of
/// the pure variant where every relocation is counted as
/// `skipped_no_region` because the caller didn't pre-load the
/// right sections.
///
/// Sections are added in iteration order of `obj.sections()`,
/// each appended once even when multiple relocs target the same
/// section.  An ELF section that has no file-backed bytes
/// (`SHT_NOBITS`, e.g. `.bss`) is *not* added — there's nothing
/// to patch — and the corresponding relocs still increment
/// `skipped_no_region` from inside the inner call.
///
/// The dedup check is per-site (does any region cover this exact
/// `site_addr`), not per-section.  On a well-formed ELF every
/// `SHF_ALLOC` section has a disjoint address range, so two
/// missing sites in the same section get unified to one staged
/// `MemRegion`.  On a malformed/synthesised ELF where two
/// `SHF_ALLOC` sections overlap, both can end up staged for two
/// different sites — `MemRegionsLookupTable`'s "last one
/// inserted wins" rule resolves the subsequent reads.
///
/// # Errors
///
/// Same as [`apply_elf_relocations`].  The lazy-load step itself
/// only fails on a malformed ELF (a section whose `data()` can't
/// be read or whose `address() + len()` overflows `u64`).
pub fn apply_elf_relocations_autoload(
    regions: &mut Vec<MemRegion>,
    obj: &object::File<'_>,
) -> Result<RelocationStats> {
    let Some(dyn_relocs) = obj.dynamic_relocations() else {
        return Ok(RelocationStats::default());
    };

    // Pass 1 — collect site addresses not yet covered, look up
    // their owning section, and stage one MemRegion per unique
    // missing section.  We never mutate `regions` here so an
    // error mid-pass leaves it untouched.
    let mut staged: Vec<MemRegion> = Vec::new();
    for (site_addr, _reloc) in dyn_relocs {
        let already_covered = regions
            .iter()
            .chain(staged.iter())
            .any(|r| r.contains(site_addr));
        if already_covered {
            continue;
        }
        let Some(sec) = find_loadable_section_containing(obj, site_addr) else {
            continue;
        };
        let data = sec.data().context("failed to parse ELF")?;
        if data.is_empty() {
            continue;
        }
        staged.push(MemRegion::new(sec.address(), data.to_vec())?);
    }

    regions.extend(staged);
    apply_elf_relocations(regions, obj)
}

/// Returns the first section in `obj` that contains `addr` and is
/// safe to materialise as a `MemRegion`: `SHF_ALLOC` set, file-
/// backed (i.e. *not* `SHT_NOBITS`).  Returns `None` when no
/// section matches — caller treats that as "leave the reloc as
/// skipped_no_region".
fn find_loadable_section_containing<'data, 'a>(
    obj: &'a object::File<'data>,
    addr: u64,
) -> Option<object::read::Section<'data, 'a>> {
    obj.sections().find(|sec| {
        let object::read::SectionFlags::Elf { sh_flags } = sec.flags() else {
            return false;
        };
        if sh_flags & u64::from(object::elf::SHF_ALLOC) == 0 {
            return false;
        }
        if sec.data().map(|d| d.is_empty()).unwrap_or(true) {
            return false;
        }
        let lo = sec.address();
        let hi = lo.saturating_add(sec.size());
        addr >= lo && addr < hi
    })
}

/// Increment `stats.skipped_unsupported_kind` and record the raw
/// ELF `r_type` of `reloc` (deduped, sorted) onto
/// `stats.unsupported_r_types` so callers can self-diagnose which
/// kinds their binary uses that we don't model.  No-op when the
/// reloc isn't ELF-flavoured.
fn record_unsupported(reloc: &object::Relocation, stats: &mut RelocationStats) {
    stats.skipped_unsupported_kind += 1;
    if let RelocationFlags::Elf { r_type } = reloc.flags()
        && let Err(idx) = stats.unsupported_r_types.binary_search(&r_type)
    {
        stats.unsupported_r_types.insert(idx, r_type);
    }
}

/// Detects the `R_*_RELATIVE` and `R_*_IRELATIVE` families —
/// image-base + addend relocations that have no symbol/section
/// target.  Returns `Some((value_to_write, size_bytes))` when
/// matched.  Image base is modelled as 0 (the binary's link-time
/// virtual address layout), so the value is just the addend.
///
/// The `r_type` constants for `R_X86_64_RELATIVE` and
/// `R_386_RELATIVE` collide (both = 8), as do several others across
/// arches, so we dispatch on the file's `Architecture` first and
/// only check `r_type` against the appropriate arch's constant.
///
/// `IRELATIVE` is the IFUNC-resolver variant: its addend is the
/// address of an indirect-resolver function the dynamic linker
/// would call to compute the actual slot value at runtime.  For
/// static analysis we write the resolver's address into the slot
/// — that's what the analyser sees in lieu of running the
/// resolver.  Treating IRELATIVE the same as RELATIVE is the
/// soundest static approximation.
fn image_relative_reloc(
    reloc: &object::Relocation,
    arch: object::Architecture,
) -> Option<(u64, usize)> {
    // R_*_RELATIVE / IRELATIVE come through with an `Absolute`
    // target (no symbol) and an `Unknown` kind (object 0.38 doesn't
    // enumerate them), so we look at the raw type code.
    let RelocationFlags::Elf { r_type } = reloc.flags() else {
        return None;
    };
    use object::Architecture as A;
    let size_bytes = match arch {
        A::X86_64
            if r_type == object::elf::R_X86_64_RELATIVE
                || r_type == object::elf::R_X86_64_IRELATIVE =>
        {
            8
        }
        A::I386
            if r_type == object::elf::R_386_RELATIVE
                || r_type == object::elf::R_386_IRELATIVE =>
        {
            4
        }
        A::Aarch64
            if r_type == object::elf::R_AARCH64_RELATIVE
                || r_type == object::elf::R_AARCH64_IRELATIVE =>
        {
            8
        }
        A::Arm
            if r_type == object::elf::R_ARM_RELATIVE
                || r_type == object::elf::R_ARM_IRELATIVE =>
        {
            4
        }
        _ => return None,
    };
    // For image-relative, the addend is the resolved value (image
    // base = 0).  Addends are i64 in object's API but represent
    // unsigned virtual addresses for these types — bitcast.
    Some((reloc.addend() as u64, size_bytes))
}

/// Detects the GOT/PLT-slot relocation family — relocations whose
/// runtime semantics are "write the symbol's address (S) at the
/// site", with no PC subtraction or addend mixing.  These are the
/// `R_*_GLOB_DAT` (GOT data slot) and `R_*_JUMP_SLOT` (PLT lazy-bind
/// slot) types.  At analysis time we resolve them eagerly: the
/// symbol's address goes into the slot, so a `Load(GOT[...])` reads
/// the real target and indirect-call patterns work without a PLT
/// model.
///
/// Object 0.38 reports both as `RelocationKind::Unknown` with
/// `size = 0`, so the size is determined by the arch (8 bytes on
/// 64-bit, 4 bytes on 32-bit).  Returns `Some(size_bytes)` when the
/// relocation is one of the recognised GLOB_DAT / JUMP_SLOT types
/// AND has a symbol target — caller is responsible for computing
/// the value (`target_addr + addend`).
fn got_or_plt_slot_reloc_size(
    reloc: &object::Relocation,
    arch: object::Architecture,
) -> Option<usize> {
    let RelocationFlags::Elf { r_type } = reloc.flags() else {
        return None;
    };
    use object::Architecture as A;
    match arch {
        A::X86_64
            if r_type == object::elf::R_X86_64_GLOB_DAT
                || r_type == object::elf::R_X86_64_JUMP_SLOT =>
        {
            Some(8)
        }
        A::I386
            if r_type == object::elf::R_386_GLOB_DAT
                || r_type == object::elf::R_386_JMP_SLOT =>
        {
            Some(4)
        }
        A::Aarch64
            if r_type == object::elf::R_AARCH64_GLOB_DAT
                || r_type == object::elf::R_AARCH64_JUMP_SLOT =>
        {
            Some(8)
        }
        A::Arm
            if r_type == object::elf::R_ARM_GLOB_DAT
                || r_type == object::elf::R_ARM_JUMP_SLOT =>
        {
            Some(4)
        }
        _ => None,
    }
}

/// Writes `value`'s low `size_bytes` bytes into `bytes` starting at
/// `off`, using the target's endianness.  Caller must guarantee
/// `off + size_bytes <= bytes.len()`.
fn write_at(bytes: &mut [u8], off: usize, value: u64, size_bytes: usize, endian_le: bool) {
    // Truncate `value` to the field width; signed/unsigned doesn't
    // matter for fixed-width 2's-complement bit patterns.
    let v_bytes = value.to_le_bytes();
    if endian_le {
        bytes[off..off + size_bytes].copy_from_slice(&v_bytes[..size_bytes]);
    } else {
        // Big-endian: write the low N bytes most-significant-first.
        for i in 0..size_bytes {
            bytes[off + i] = v_bytes[size_bytes - 1 - i];
        }
    }
}

/// Convenience: load every allocatable file-backed section (via
/// [`elf_get_allocatable_file_backed_sections_as_mem_regions`])
/// and apply dynamic relocations to the resulting regions in-place.
///
/// Use this when you want analysis-grade fidelity for an ET_DYN
/// binary: code, rodata, and writable-but-relocated data
/// (`.data.rel.ro`, `.got`) all land in the returned regions with
/// every applicable relocation patched in.
///
/// # Errors
///
/// Propagates any error from the inner helpers; relocation
/// resolution itself only errors on a malformed ELF.
pub fn elf_load_with_relocations(
    obj: &object::File<'_>,
) -> Result<(Vec<MemRegion>, RelocationStats)> {
    let mut regions = elf_get_allocatable_file_backed_sections_as_mem_regions(obj)?;
    let stats = apply_elf_relocations(&mut regions, obj)?;
    Ok((regions, stats))
}

// ── load_elf ──────────────────────────────────────────────────────────────────

/// Loads and parses an ELF file from `path`, returning a `'static` reference.
///
/// On success, the file bytes are read into a `Box<[u8]>` that is then
/// intentionally **leaked** so the returned `object::File<'static>` remains
/// valid for the lifetime of the process. This is suitable for tests and
/// short-lived CLI tools where the cost of a one-time leak is acceptable.
///
/// On error, no bytes are leaked: the file is read, validated by an
/// in-place parse, and only on parse success are the bytes promoted to
/// `'static` (a second parse — guaranteed to succeed since the bytes are
/// identical — produces the returned `object::File`).
///
/// Callers that only need an [`ElfFileMemReader`] should prefer
/// [`ElfFileMemReader::from_path`], which does not leak.
///
/// # Errors
///
/// Returns an error if the file cannot be read from disk or if
/// the bytes fail to parse as a valid ELF.
pub fn load_elf<P: AsRef<std::path::Path>>(path: P) -> Result<object::File<'static>> {
    let data = std::fs::read(path).context("failed to read file")?;
    // Validate the parse on a borrowed view BEFORE leaking. If the bytes
    // don't parse as ELF, we drop `data` normally instead of leaking it
    // onto the heap forever for nothing — the leak only pays for itself
    // when we actually return an `object::File<'static>`.
    object::File::parse(&data[..]).context("failed to parse ELF")?;
    let leaked: &'static [u8] = Box::leak(data.into_boxed_slice());
    // Re-parse the (now `'static`) bytes. Identical bytes parse identically,
    // so this `?` cannot fail in practice; we still propagate via `?` to
    // avoid `expect`/`unwrap` (forbidden in this crate).
    object::File::parse(leaked).context("failed to parse ELF")
}

