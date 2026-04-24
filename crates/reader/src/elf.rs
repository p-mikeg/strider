//! ELF-backed implementation of [`crate::MemRegion`]s and the
//! [`rsleigh::MemReader`] trait.
//!
//! This module is the ELF-specific half of the `reader` crate. The generic
//! region-lookup machinery (`MemRegion`, `MemRegionsLookupTable`) lives in
//! [`crate`] so other backends (raw blobs, PE, Mach-O, …) can reuse it.

use object::{Object, ObjectSection, ObjectSegment};

use crate::{MemRegion, MemRegionsLookupTable, Result, error};

// ── ELF → MemRegion converters ────────────────────────────────────────────────

/// Converts a single ELF segment into a [`MemRegion`].
///
/// # Errors
///
/// Returns an error wrapping the underlying `object::Error` if the
/// segment's file-backed data cannot be read.
pub fn elf_segment_to_mem_region(segment: &object::read::Segment<'_, '_>) -> Result<MemRegion> {
    MemRegion::new(segment.address(), segment.data()?.to_vec())
}

/// Converts a single ELF section into a [`MemRegion`].
///
/// # Errors
///
/// Returns an error wrapping the underlying `object::Error` if the
/// section's file-backed data cannot be read.
pub fn elf_section_to_mem_region(section: &object::read::Section<'_, '_>) -> Result<MemRegion> {
    MemRegion::new(section.address(), section.data()?.to_vec())
}

/// Collects ELF segments into [`MemRegion`]s, keeping only those for which
/// `filter` returns `true`.
///
/// Segments with empty data are skipped. Preserves iteration order;
/// duplicate `start_addr`s are resolved later by [`MemRegionsLookupTable`]
/// under its "last one inserted wins" rule.
///
/// # Errors
///
/// Currently infallible after filtering (segments that fail to read are
/// skipped), but preserves a `Result` return for future backends that may
/// need to surface a parse error through `object::Error`.
pub fn elf_segments_to_mem_regions(
    obj: &object::File<'_>,
    filter: impl Fn(&object::read::Segment<'_, '_>) -> bool,
) -> Result<Vec<MemRegion>> {
    let mut out = Vec::new();
    for seg in obj.segments() {
        let Ok(data) = seg.data() else { continue };
        if data.is_empty() || !filter(&seg) {
            continue;
        }
        out.push(MemRegion::new(seg.address(), data.to_vec())?);
    }
    Ok(out)
}

/// Collects ELF sections into [`MemRegion`]s, keeping only those for which
/// `filter` returns `true`.
///
/// Sections whose `data()` call fails or returns empty bytes are always
/// skipped (this excludes `SHT_NOBITS` sections like `.bss`). Preserves
/// iteration order; duplicate `start_addr`s are resolved later by
/// [`MemRegionsLookupTable`] under its "last one inserted wins" rule.
///
/// # Errors
///
/// Currently infallible after filtering (sections that fail to read are
/// skipped), but preserves a `Result` return for future backends that may
/// need to surface a parse error through `object::Error`.
pub fn elf_sections_to_mem_regions(
    obj: &object::File<'_>,
    filter: impl Fn(&object::read::Section<'_, '_>) -> bool,
) -> Result<Vec<MemRegion>> {
    let mut out = Vec::new();
    for sec in obj.sections() {
        let Ok(data) = sec.data() else { continue };
        if data.is_empty() || !filter(&sec) {
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
/// Propagates any `object::Error` from the underlying section/segment
/// iteration; see [`elf_segments_to_mem_regions`].
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
/// Propagates any `object::Error` from the underlying section iteration;
/// see [`elf_sections_to_mem_regions`].
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
/// Propagates any `object::Error` from the underlying section iteration;
/// see [`elf_sections_to_mem_regions`].
pub fn elf_get_code_and_readonly_sections_as_mem_regions(
    obj: &object::File<'_>,
) -> Result<Vec<MemRegion>> {
    elf_sections_to_mem_regions(obj, section_is_code_or_readonly)
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
    endianness: object::Endianness,
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
    /// Propagates any `object::Error` from reading the selected sections.
    pub fn from_object(obj: &object::File<'_>) -> Result<Self> {
        let regions = elf_get_code_and_readonly_sections_as_mem_regions(obj)?;
        Ok(Self {
            lookup: MemRegionsLookupTable::new(regions),
            endianness: obj.endianness(),
        })
    }

    /// Builds a reader by parsing the given ELF bytes.
    ///
    /// The bytes are parsed in-place; no leak is required.
    ///
    /// # Errors
    ///
    /// Returns `ErrorKind::Object` if the bytes fail to parse as a valid
    /// ELF, or any error produced by [`from_object`](Self::from_object).
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        let obj = object::File::parse(bytes)?;
        Self::from_object(&obj)
    }

    /// Builds a reader by reading and parsing an ELF file from disk.
    ///
    /// # Errors
    ///
    /// Returns `ErrorKind::Io` if the file cannot be read from disk, or
    /// any error produced by [`from_bytes`](Self::from_bytes).
    pub fn from_path<P: AsRef<std::path::Path>>(path: P) -> Result<Self> {
        let bytes = std::fs::read(path)?;
        Self::from_bytes(&bytes)
    }
}

impl rsleigh::MemReader for ElfFileMemReader {
    type Err = crate::Error;

    fn read(&self, addr: rsleigh::VnAddr, out_buf: &mut [u8]) -> Result<usize> {
        self.lookup
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
        let n = self.lookup.read(addr, slot)?;
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
///
/// # Errors
///
/// Returns `ErrorKind::Io` if the file cannot be read from disk, or
/// `ErrorKind::Object` if the bytes fail to parse as a valid ELF.
pub fn load_elf<P: AsRef<std::path::Path>>(path: P) -> Result<object::File<'static>> {
    let data = std::fs::read(path)?;
    let leaked: &'static [u8] = Box::leak(data.into_boxed_slice());
    Ok(object::File::parse(leaked)?)
}

