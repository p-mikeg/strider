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
/// `SHT_NOBITS` section.
pub fn elf_get_code_and_readonly_sections_as_mem_regions(
    obj: &object::File<'_>,
) -> Result<Vec<MemRegion>> {
    elf_sections_to_mem_regions(obj, |sec| {
        let object::read::SectionFlags::Elf { sh_flags } = sec.flags() else {
            return false;
        };
        let is_exec = sh_flags & object::elf::SHF_EXECINSTR as u64 != 0;
        let is_writable = sh_flags & object::elf::SHF_WRITE as u64 != 0;
        is_exec || !is_writable
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

    /// Test-only constructor that takes already-built parts.
    ///
    /// Useful for unit-testing the trait impls without a real ELF file.
    #[cfg(test)]
    pub(crate) fn from_parts(
        regions_mem_reader: RegionsMemReader,
        endianness: object::Endianness,
    ) -> Self {
        Self {
            regions_mem_reader,
            endianness,
        }
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

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ReadOnlyMemory;

    // ── test helpers ──────────────────────────────────────────────────────────

    /// Builds an `ElfFileMemReader` from a single synthetic region at
    /// `start`, with `bytes` as its content and the given endianness.
    fn reader_with(
        start: u64,
        bytes: Vec<u8>,
        endianness: object::Endianness,
    ) -> ElfFileMemReader {
        let region = MemRegion::new(start, bytes);
        let lookup = MemRegionsLookupTable::new([region]);
        let regions = RegionsMemReader::new(lookup);
        ElfFileMemReader::from_parts(regions, endianness)
    }

    // ── ReadOnlyMemory: space filter ──────────────────────────────────────────

    /// Only `VnSpace::RAM` produces a hit; other spaces always return `None`.
    #[test]
    fn ro_read_non_ram_space_returns_none() {
        let r = reader_with(0x1000, vec![1, 2, 3, 4], object::Endianness::Little);
        assert_eq!(ReadOnlyMemory::read(&r, rsleigh::VnSpace::REGISTER, 0x1000, 4), None);
        assert_eq!(ReadOnlyMemory::read(&r, rsleigh::VnSpace::UNIQUE, 0x1000, 4), None);
        assert_eq!(ReadOnlyMemory::read(&r, rsleigh::VnSpace::CONST, 0x1000, 4), None);
    }

    // ── ReadOnlyMemory: size bounds ───────────────────────────────────────────

    /// `size == 0` is not a legitimate load; the trait returns `None`.
    #[test]
    fn ro_read_size_zero_returns_none() {
        let r = reader_with(0x1000, vec![1, 2, 3, 4], object::Endianness::Little);
        assert_eq!(ReadOnlyMemory::read(&r, rsleigh::VnSpace::RAM, 0x1000, 0), None);
    }

    /// `size > 8` exceeds what a `u64` can carry; the trait returns `None`.
    #[test]
    fn ro_read_size_greater_than_eight_returns_none() {
        let r = reader_with(0x1000, vec![0u8; 16], object::Endianness::Little);
        assert_eq!(ReadOnlyMemory::read(&r, rsleigh::VnSpace::RAM, 0x1000, 9), None);
    }

    // ── ReadOnlyMemory: partial read ──────────────────────────────────────────

    /// When the region can only supply a prefix of the requested bytes,
    /// return `None` instead of truncated data.
    #[test]
    fn ro_read_partial_region_returns_none() {
        // region covers 0x1000..0x1004 (4 bytes)
        let r = reader_with(0x1000, vec![1, 2, 3, 4], object::Endianness::Little);
        // request 4 bytes starting 2 bytes before the end → only 2 available
        assert_eq!(ReadOnlyMemory::read(&r, rsleigh::VnSpace::RAM, 0x1002, 4), None);
    }

    /// An address outside any region returns `None`.
    #[test]
    fn ro_read_unmapped_address_returns_none() {
        let r = reader_with(0x1000, vec![1, 2, 3, 4], object::Endianness::Little);
        assert_eq!(ReadOnlyMemory::read(&r, rsleigh::VnSpace::RAM, 0x9000, 4), None);
    }

    // ── ReadOnlyMemory: endianness ────────────────────────────────────────────

    /// 4 bytes `01 02 03 04` as little-endian u32 = 0x04030201.
    #[test]
    fn ro_read_little_endian_u32() {
        let r = reader_with(0x1000, vec![0x01, 0x02, 0x03, 0x04], object::Endianness::Little);
        assert_eq!(
            ReadOnlyMemory::read(&r, rsleigh::VnSpace::RAM, 0x1000, 4),
            Some(0x04030201)
        );
    }

    /// 4 bytes `01 02 03 04` as big-endian u32 = 0x01020304.
    #[test]
    fn ro_read_big_endian_u32() {
        let r = reader_with(0x1000, vec![0x01, 0x02, 0x03, 0x04], object::Endianness::Big);
        assert_eq!(
            ReadOnlyMemory::read(&r, rsleigh::VnSpace::RAM, 0x1000, 4),
            Some(0x01020304)
        );
    }

    /// 8-byte read picks up the full u64 with the correct endianness.
    #[test]
    fn ro_read_little_endian_u64() {
        let r = reader_with(
            0x1000,
            vec![0x78, 0x56, 0x34, 0x12, 0xef, 0xcd, 0xab, 0x89],
            object::Endianness::Little,
        );
        assert_eq!(
            ReadOnlyMemory::read(&r, rsleigh::VnSpace::RAM, 0x1000, 8),
            Some(0x89abcdef12345678)
        );
    }

    /// 1-byte reads do not depend on endianness.
    #[test]
    fn ro_read_single_byte() {
        let r = reader_with(0x1000, vec![0xab], object::Endianness::Little);
        assert_eq!(
            ReadOnlyMemory::read(&r, rsleigh::VnSpace::RAM, 0x1000, 1),
            Some(0xab)
        );
    }
}
