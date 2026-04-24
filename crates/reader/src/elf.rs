//! ELF-backed implementation of [`crate::MemRegion`]s and the
//! [`rsleigh::MemReader`] trait.
//!
//! This module is the ELF-specific half of the `reader` crate. The generic
//! region-lookup machinery (`MemRegion`, `MemRegionsLookupTable`,
//! `RegionsMemReader`) lives in [`crate`] so other backends (raw blobs, PE,
//! Mach-O, …) can reuse it.

use std::collections::BTreeMap;

use object::{Object, ObjectSection, ObjectSegment};

use crate::{MemRegion, MemRegionsLookupTable, RegionsMemReader, Result, error};

// ── ELF → MemRegion converters ────────────────────────────────────────────────

/// Converts a single ELF segment into a [`MemRegion`].
pub fn elf_segment_to_mem_region(segment: &object::read::Segment<'_, '_>) -> Result<MemRegion> {
    Ok(MemRegion::new(segment.address(), segment.data()?.to_vec()))
}

/// Converts a single ELF section into a [`MemRegion`].
pub fn elf_section_to_mem_region(section: &object::read::Section<'_, '_>) -> Result<MemRegion> {
    Ok(MemRegion::new(section.address(), section.data()?.to_vec()))
}

/// Collects ELF segments into [`MemRegion`]s, keeping only those for which
/// `filter` returns `true`.
///
/// Segments with empty data are always skipped.  If two segments share the
/// same start address, the last one encountered is kept.
pub fn elf_segments_to_mem_regions(
    obj: &object::File<'_>,
    filter: impl Fn(&object::read::Segment<'_, '_>) -> bool,
) -> Result<Vec<MemRegion>> {
    let mut by_start: BTreeMap<u64, MemRegion> = BTreeMap::new();

    for seg in obj.segments() {
        let Ok(data) = seg.data() else { continue };
        if data.is_empty() || !filter(&seg) {
            continue;
        }
        let region = elf_segment_to_mem_region(&seg)?;
        by_start.insert(region.start_addr, region);
    }

    Ok(by_start.into_values().collect())
}

/// Collects ELF sections into [`MemRegion`]s, keeping only those for which
/// `filter` returns `true`.
///
/// Sections whose `data()` call fails or returns empty bytes are always
/// skipped (this excludes `SHT_NOBITS` sections like `.bss`). If two sections
/// share the same start address, the last one encountered is kept.
pub fn elf_sections_to_mem_regions(
    obj: &object::File<'_>,
    filter: impl Fn(&object::read::Section<'_, '_>) -> bool,
) -> Result<Vec<MemRegion>> {
    let mut by_start: BTreeMap<u64, MemRegion> = BTreeMap::new();

    for sec in obj.sections() {
        let Ok(data) = sec.data() else { continue };
        if data.is_empty() || !filter(&sec) {
            continue;
        }
        let region = elf_section_to_mem_region(&sec)?;
        by_start.insert(region.start_addr, region);
    }

    Ok(by_start.into_values().collect())
}

// ── Executable-only helpers ───────────────────────────────────────────────────

/// Returns all executable ELF segments (i.e. those with the `PF_X` flag set)
/// as [`MemRegion`]s.
pub fn elf_get_executable_segments_as_mem_regions(
    obj: &object::File<'_>,
) -> Result<Vec<MemRegion>> {
    elf_segments_to_mem_regions(obj, |seg| {
        matches!(
            seg.flags(),
            object::read::SegmentFlags::Elf { p_flags }
                if p_flags & object::elf::PF_X != 0
        )
    })
}

/// Returns all executable ELF sections (i.e. those with `SHF_EXECINSTR` set)
/// as [`MemRegion`]s.
pub fn elf_get_executable_sections_as_mem_regions(
    obj: &object::File<'_>,
) -> Result<Vec<MemRegion>> {
    elf_sections_to_mem_regions(obj, |sec| {
        matches!(
            sec.flags(),
            object::read::SectionFlags::Elf { sh_flags }
                if sh_flags & object::elf::SHF_EXECINSTR as u64 != 0
        )
    })
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
pub fn elf_get_code_and_readonly_sections_as_mem_regions(
    obj: &object::File<'_>,
) -> Result<Vec<MemRegion>> {
    elf_sections_to_mem_regions(obj, |sec| {
        let object::read::SectionFlags::Elf { sh_flags } = sec.flags() else {
            return false;
        };
        let is_alloc = sh_flags & object::elf::SHF_ALLOC as u64 != 0;
        let is_exec = sh_flags & object::elf::SHF_EXECINSTR as u64 != 0;
        let is_writable = sh_flags & object::elf::SHF_WRITE as u64 != 0;
        is_alloc && (is_exec || !is_writable)
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
    /// In-memory representation of the mapped regions.
    pub regions_mem_reader: RegionsMemReader,
    /// Endianness of the source ELF. Used by the [`crate::ReadOnlyMemory`]
    /// impl when assembling bytes into a `u64`.
    pub endianness: object::Endianness,
}

impl ElfFileMemReader {
    /// Builds a reader from an already-parsed [`object::File`].
    ///
    /// Loads every executable section and every non-writable section with
    /// file-backed data. The parsed object is not retained — the returned
    /// reader is self-owning.
    pub fn from_object(obj: &object::File<'_>) -> Result<Self> {
        let regions = elf_get_code_and_readonly_sections_as_mem_regions(obj)?;
        let lookup = MemRegionsLookupTable::new(regions);
        Ok(Self {
            regions_mem_reader: RegionsMemReader::new(lookup),
            endianness: obj.endianness(),
        })
    }

    /// Builds a reader by parsing the given ELF bytes.
    ///
    /// The bytes are parsed in-place; no leak is required.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        let obj = object::File::parse(bytes)?;
        Self::from_object(&obj)
    }

    /// Builds a reader by reading and parsing an ELF file from disk.
    pub fn from_path<P: AsRef<std::path::Path>>(path: P) -> Result<Self> {
        let bytes = std::fs::read(path)?;
        Self::from_bytes(&bytes)
    }

    /// Builds a reader from the executable **segments** of `obj`.
    ///
    /// Kept for callers that need the legacy exec-segments-only behaviour. Most
    /// callers should use [`ElfFileMemReader::from_object`] instead.
    pub fn from_elf_segments(obj: &object::File<'_>) -> Result<Self> {
        let regions = elf_get_executable_segments_as_mem_regions(obj)?;
        let lookup = MemRegionsLookupTable::new(regions);
        Ok(Self {
            regions_mem_reader: RegionsMemReader::new(lookup),
            endianness: obj.endianness(),
        })
    }

    /// Builds a reader from the executable **sections** of `obj`.
    ///
    /// Kept for callers that need the legacy exec-sections-only behaviour.
    /// Most callers should use [`ElfFileMemReader::from_object`] instead.
    pub fn from_elf_sections(obj: &object::File<'_>) -> Result<Self> {
        let regions = elf_get_executable_sections_as_mem_regions(obj)?;
        let lookup = MemRegionsLookupTable::new(regions);
        Ok(Self {
            regions_mem_reader: RegionsMemReader::new(lookup),
            endianness: obj.endianness(),
        })
    }
}

impl rsleigh::MemReader for ElfFileMemReader {
    type Err = crate::Error;

    fn read(&self, addr: rsleigh::VnAddr, out_buf: &mut [u8]) -> Result<usize> {
        self.regions_mem_reader
            .read(addr.off, out_buf)
            .ok_or_else(|| error::ErrorKind::NotMapped(addr.off).into())
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
        // buffer so from_le_bytes / from_be_bytes produce the same numeric
        // value for an N-byte load as the target machine would.
        let mut buf = [0u8; 8];
        let slot = match self.endianness {
            object::Endianness::Little => &mut buf[..size],
            object::Endianness::Big => &mut buf[8 - size..],
        };
        let n = self.regions_mem_reader.read(addr, slot)?;
        if n != size {
            return None;
        }
        let val = match self.endianness {
            object::Endianness::Little => u64::from_le_bytes(buf),
            object::Endianness::Big => u64::from_be_bytes(buf),
        };
        Some(val)
    }
}

// ── load_elf ──────────────────────────────────────────────────────────────────

/// Loads and parses an ELF file from `path`, returning a `'static` reference.
///
/// The file bytes are read into a `Box<[u8]>` that is then intentionally
/// **leaked** so the returned `object::File<'static>` remains valid for the
/// lifetime of the process.  This is suitable for tests and short-lived CLI
/// tools where the cost of a one-time leak is acceptable.
///
/// Callers that only need an [`ElfFileMemReader`] should prefer
/// [`ElfFileMemReader::from_path`], which does not leak.
pub fn load_elf(path: &str) -> Result<object::File<'static>> {
    let data = std::fs::read(path)?;
    let leaked: &'static [u8] = Box::leak(data.into_boxed_slice());
    Ok(object::File::parse(leaked)?)
}

