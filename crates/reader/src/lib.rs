//! ELF binary memory reader for rsleigh.
//!
//! This crate bridges ELF files (parsed by `object`) and rsleigh's
//! [`MemReader`] trait so that the Sleigh disassembler can read instruction
//! bytes directly from ELF sections or segments.
//!
//! # Architecture
//!
//! ```text
//! ELF file on disk
//!   └── object::File (parsed)
//!         ├── sections / segments  ──►  Vec<MemRegion>
//!         │                                  │
//!         │                                  ▼
//!         │                        MemRegionsLookupTable (BTreeMap)
//!         │                                  │
//!         │                                  ▼
//!         └── ElfFileMemReader ──── RegionsMemReader ──► rsleigh::MemReader
//! ```
//!
//! The typical entry point is [`ElfFileMemReader::from_elf_sections`], which
//! maps all executable ELF sections into memory and hands the reader to
//! `rsleigh::Sleigh::new`.

use std::collections::BTreeMap;

use object::{Object, ObjectSection, ObjectSegment};
use thiserror::Error;

// ── MemRegion ─────────────────────────────────────────────────────────────────

/// A contiguous range of bytes loaded at a fixed virtual address.
///
/// Corresponds to one ELF section or segment mapped into the virtual address
/// space of the target binary.
#[derive(Clone, Debug)]
pub struct MemRegion {
    /// First virtual address covered by this region.
    pub start_addr: u64,
    /// Raw bytes of the region, starting at `start_addr`.
    pub data: Vec<u8>,
}

impl MemRegion {
    /// Creates a new `MemRegion` loaded at `start_addr`.
    pub fn new(start_addr: u64, data: Vec<u8>) -> Self {
        Self { start_addr, data }
    }

    /// One past the last virtual address covered by this region.
    ///
    /// `end_addr == start_addr + data.len()`.
    #[must_use]
    pub fn end_addr(&self) -> u64 {
        self.start_addr + self.data.len() as u64
    }

    /// Returns `true` when `addr` falls within `[start_addr, end_addr)`.
    #[must_use]
    pub fn contains(&self, addr: u64) -> bool {
        addr >= self.start_addr && addr < self.end_addr()
    }

    /// Reads bytes starting at `addr` into `out`.
    ///
    /// Returns the number of bytes copied, which may be less than `out.len()`
    /// if `addr + out.len()` extends past the end of this region.
    ///
    /// Returns `None` when `addr` is not within this region at all.
    pub fn read(&self, addr: u64, out: &mut [u8]) -> Option<usize> {
        if !self.contains(addr) {
            return None;
        }

        let offset = (addr - self.start_addr) as usize;
        let available = self.data.len() - offset; // safe: contains() guarantees offset < len
        let to_copy = available.min(out.len());

        out[..to_copy].copy_from_slice(&self.data[offset..offset + to_copy]);

        Some(to_copy)
    }
}

// ── MemRegionsLookupTable ─────────────────────────────────────────────────────

/// A fast lookup table over a collection of non-overlapping [`MemRegion`]s.
///
/// Regions are indexed by their start address in a `BTreeMap`, allowing an
/// O(log n) candidate lookup via a range query.  When two regions have the
/// same start address the last one inserted wins.
#[derive(Debug)]
pub struct MemRegionsLookupTable {
    /// Sorted map from region start address to the region itself.
    regions: BTreeMap<u64, MemRegion>,
}

impl MemRegionsLookupTable {
    /// Builds a lookup table from `regions`.
    ///
    /// If two regions share the same start address, the later one in iteration
    /// order overwrites the earlier one.
    pub fn new<I: IntoIterator<Item = MemRegion>>(regions: I) -> Self {
        let mut map = BTreeMap::new();
        for region in regions {
            map.insert(region.start_addr, region);
        }
        Self { regions: map }
    }

    /// Reads bytes starting at `addr` from whichever region contains it.
    ///
    /// Returns `None` when no region contains `addr`.
    /// Partial reads are possible — see [`MemRegion::read`].
    pub fn read(&self, addr: u64, out: &mut [u8]) -> Option<usize> {
        // Find the last region whose start_addr <= addr, then confirm addr is
        // actually inside it (start_addr alone is not sufficient).
        let (_, region) = self.regions.range(..=addr).next_back()?;
        region.read(addr, out)
    }
}

// ── RegionsMemReader ──────────────────────────────────────────────────────────

/// A thin wrapper around [`MemRegionsLookupTable`] that provides a `read`
/// method for convenient use in higher-level code.
///
/// [`ElfFileMemReader`] uses this internally; most callers should interact
/// with `ElfFileMemReader` directly.
#[derive(Debug)]
pub struct RegionsMemReader {
    lookup: MemRegionsLookupTable,
}

impl RegionsMemReader {
    /// Creates a reader backed by the given lookup table.
    pub fn new(lookup: MemRegionsLookupTable) -> Self {
        Self { lookup }
    }

    /// Reads bytes from the region containing `addr`.
    ///
    /// Returns `None` when `addr` is not mapped.  Partial reads are possible.
    pub fn read(&self, addr: u64, out: &mut [u8]) -> Option<usize> {
        self.lookup.read(addr, out)
    }
}

// ── ELF → MemRegion converters ────────────────────────────────────────────────

/// Converts a single ELF segment into a [`MemRegion`].
pub fn elf_segment_to_mem_region(
    segment: &object::read::Segment<'_, '_>,
) -> Result<MemRegion, object::Error> {
    Ok(MemRegion::new(segment.address(), segment.data()?.to_vec()))
}

/// Converts a single ELF section into a [`MemRegion`].
pub fn elf_section_to_mem_region(
    section: &object::read::Section<'_, '_>,
) -> Result<MemRegion, object::Error> {
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
) -> Result<Vec<MemRegion>, object::Error> {
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
/// If two sections share the same start address, the last one encountered is
/// kept.
pub fn elf_sections_to_mem_regions(
    obj: &object::File<'_>,
    filter: impl Fn(&object::read::Section<'_, '_>) -> bool,
) -> Result<Vec<MemRegion>, object::Error> {
    let mut by_start: BTreeMap<u64, MemRegion> = BTreeMap::new();

    for sec in obj.sections() {
        if !filter(&sec) {
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
) -> Result<Vec<MemRegion>, object::Error> {
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
) -> Result<Vec<MemRegion>, object::Error> {
    elf_sections_to_mem_regions(obj, |sec| {
        matches!(
            sec.flags(),
            object::read::SectionFlags::Elf { sh_flags }
                if sh_flags & object::elf::SHF_EXECINSTR as u64 != 0
        )
    })
}

// ── ElfFileMemReader ──────────────────────────────────────────────────────────

/// An rsleigh [`MemReader`] backed by an ELF file's sections or segments.
///
/// Holds a reference to the parsed [`object::File`] (for symbol lookups etc.)
/// alongside the loaded memory regions used for instruction reads.
///
/// # Lifetimes
/// - `'a` — lifetime of the borrow of the `object::File`.
/// - `'data` — lifetime of the underlying byte buffer that `object::File`
///   parses from.
#[derive(Debug)]
pub struct ElfFileMemReader<'a, 'data> {
    /// The parsed ELF object.  Available for symbol resolution after the
    /// reader is constructed.
    pub obj: &'a object::File<'data>,
    /// In-memory representation of the mapped regions.
    pub regions_mem_reader: RegionsMemReader,
}

/// Error returned by [`ElfFileMemReader`] when a read fails.
#[derive(Debug, Error)]
pub enum ElfMemReaderError {
    /// The requested address is not mapped in any loaded region.
    #[error("address {0:#x} is not mapped")]
    NotMapped(u64),
    /// An underlying `object` crate error occurred while loading regions.
    #[error("object error: {0}")]
    Object(#[from] object::Error),
}

impl<'a, 'data> ElfFileMemReader<'a, 'data> {
    /// Creates a reader from the executable **segments** of `obj`.
    ///
    /// Segments correspond to the runtime layout (PT_LOAD entries); use this
    /// when the binary is not stripped and segments are available.
    pub fn from_elf_segments(
        obj: &'a object::File<'data>,
    ) -> Result<Self, ElfMemReaderError> {
        let regions = elf_get_executable_segments_as_mem_regions(obj)?;
        let lookup = MemRegionsLookupTable::new(regions);
        Ok(Self {
            obj,
            regions_mem_reader: RegionsMemReader::new(lookup),
        })
    }

    /// Creates a reader from the executable **sections** of `obj`.
    ///
    /// Sections provide finer-grained granularity than segments and work even
    /// when PT_LOAD entries are absent.  This is the recommended constructor
    /// for most use-cases.
    pub fn from_elf_sections(
        obj: &'a object::File<'data>,
    ) -> Result<Self, ElfMemReaderError> {
        let regions = elf_get_executable_sections_as_mem_regions(obj)?;
        let lookup = MemRegionsLookupTable::new(regions);
        Ok(Self {
            obj,
            regions_mem_reader: RegionsMemReader::new(lookup),
        })
    }
}

impl<'a, 'data> rsleigh::MemReader for ElfFileMemReader<'a, 'data> {
    type Err = ElfMemReaderError;

    fn read(&self, addr: rsleigh::VnAddr, out_buf: &mut [u8]) -> Result<usize, Self::Err> {
        self.regions_mem_reader
            .read(addr.off, out_buf)
            .ok_or(ElfMemReaderError::NotMapped(addr.off))
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
/// # Panics
///
/// Panics when the file cannot be read or cannot be parsed as an ELF.
pub fn load_elf(path: &str) -> object::File<'static> {
    let data = std::fs::read(path).expect("failed to read ELF file");
    let leaked: &'static [u8] = Box::leak(data.into_boxed_slice());
    object::File::parse(leaked).expect("failed to parse ELF file")
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── helpers ───────────────────────────────────────────────────────────────

    /// Builds a `MemRegion` at `start` with `len` bytes, each equal to its
    /// offset within the region (i.e. `data[i] == i as u8 & 0xff`).
    fn make_region(start: u64, len: usize) -> MemRegion {
        let data: Vec<u8> = (0..len).map(|i| i as u8).collect();
        MemRegion::new(start, data)
    }

    // ── MemRegion::end_addr ───────────────────────────────────────────────────

    /// `end_addr` must equal `start_addr + data.len()`.
    #[test]
    fn mem_region_end_addr() {
        let r = make_region(0x1000, 16);
        assert_eq!(r.end_addr(), 0x1010);
    }

    /// An empty region has `end_addr == start_addr`.
    #[test]
    fn mem_region_end_addr_empty() {
        let r = MemRegion::new(0x2000, vec![]);
        assert_eq!(r.end_addr(), 0x2000);
    }

    // ── MemRegion::contains ───────────────────────────────────────────────────

    /// `start_addr` itself is inside the region.
    #[test]
    fn mem_region_contains_start() {
        let r = make_region(0x1000, 16);
        assert!(r.contains(0x1000));
    }

    /// The byte just before `end_addr` is inside the region.
    #[test]
    fn mem_region_contains_last_byte() {
        let r = make_region(0x1000, 16);
        assert!(r.contains(0x100f));
    }

    /// `end_addr` itself is NOT inside the region (half-open interval).
    #[test]
    fn mem_region_does_not_contain_end_addr() {
        let r = make_region(0x1000, 16);
        assert!(!r.contains(0x1010));
    }

    /// An address below `start_addr` is not inside the region.
    #[test]
    fn mem_region_does_not_contain_before_start() {
        let r = make_region(0x1000, 16);
        assert!(!r.contains(0x0fff));
    }

    /// An empty region contains no address.
    #[test]
    fn mem_region_empty_contains_nothing() {
        let r = MemRegion::new(0x1000, vec![]);
        assert!(!r.contains(0x1000));
    }

    // ── MemRegion::read ───────────────────────────────────────────────────────

    /// A full read starting at `start_addr` returns all bytes correctly.
    #[test]
    fn mem_region_read_full_at_start() {
        let r = make_region(0x1000, 4);
        let mut buf = [0u8; 4];
        assert_eq!(r.read(0x1000, &mut buf), Some(4));
        assert_eq!(buf, [0, 1, 2, 3]);
    }

    /// A read that starts mid-region returns the correct slice.
    #[test]
    fn mem_region_read_mid_region() {
        let r = make_region(0x1000, 8);
        let mut buf = [0u8; 3];
        assert_eq!(r.read(0x1002, &mut buf), Some(3));
        assert_eq!(buf, [2, 3, 4]);
    }

    /// Reading with a buffer that extends past the region end returns a
    /// partial read (only the bytes that are available).
    #[test]
    fn mem_region_read_partial_past_end() {
        let r = make_region(0x1000, 4);
        let mut buf = [0xffu8; 8];
        assert_eq!(r.read(0x1002, &mut buf), Some(2)); // only 2 bytes left
        assert_eq!(buf[0], 2);
        assert_eq!(buf[1], 3);
        // bytes beyond the partial read are untouched
        assert_eq!(buf[2], 0xff);
    }

    /// A zero-length buffer read returns `Some(0)` (address is valid).
    #[test]
    fn mem_region_read_zero_length_buf() {
        let r = make_region(0x1000, 4);
        let mut buf: [u8; 0] = [];
        assert_eq!(r.read(0x1000, &mut buf), Some(0));
    }

    /// Reading from an address outside the region returns `None`.
    #[test]
    fn mem_region_read_outside_returns_none() {
        let r = make_region(0x1000, 4);
        let mut buf = [0u8; 4];
        assert_eq!(r.read(0x2000, &mut buf), None);
    }

    /// Reading from `end_addr` (one past the end) returns `None`.
    #[test]
    fn mem_region_read_at_end_addr_returns_none() {
        let r = make_region(0x1000, 4);
        let mut buf = [0u8; 1];
        assert_eq!(r.read(0x1004, &mut buf), None);
    }

    // ── MemRegionsLookupTable ─────────────────────────────────────────────────

    /// A lookup table with a single region finds addresses within it.
    #[test]
    fn lookup_table_finds_address_in_single_region() {
        let table = MemRegionsLookupTable::new([make_region(0x1000, 16)]);
        let mut buf = [0u8; 2];
        assert_eq!(table.read(0x1000, &mut buf), Some(2));
        assert_eq!(buf, [0, 1]);
    }

    /// An address before all regions returns `None`.
    #[test]
    fn lookup_table_miss_before_all_regions() {
        let table = MemRegionsLookupTable::new([make_region(0x1000, 16)]);
        let mut buf = [0u8; 1];
        assert_eq!(table.read(0x0fff, &mut buf), None);
    }

    /// An address after all regions returns `None`.
    #[test]
    fn lookup_table_miss_after_all_regions() {
        let table = MemRegionsLookupTable::new([make_region(0x1000, 16)]);
        let mut buf = [0u8; 1];
        assert_eq!(table.read(0x1010, &mut buf), None);
    }

    /// With two non-overlapping regions, each address is found in the correct one.
    #[test]
    fn lookup_table_two_regions_correct_dispatch() {
        let table = MemRegionsLookupTable::new([
            make_region(0x1000, 16),
            make_region(0x2000, 16),
        ]);
        let mut buf = [0u8; 1];

        // first region
        assert_eq!(table.read(0x1005, &mut buf), Some(1));
        assert_eq!(buf[0], 5);

        // second region
        assert_eq!(table.read(0x2007, &mut buf), Some(1));
        assert_eq!(buf[0], 7);
    }

    /// When two regions share the same start address, the last one wins.
    #[test]
    fn lookup_table_same_start_last_wins() {
        let mut r1 = make_region(0x1000, 4);
        r1.data = vec![0xaa, 0xaa, 0xaa, 0xaa];
        let mut r2 = make_region(0x1000, 4);
        r2.data = vec![0xbb, 0xbb, 0xbb, 0xbb];

        let table = MemRegionsLookupTable::new([r1, r2]);
        let mut buf = [0u8; 1];
        assert_eq!(table.read(0x1000, &mut buf), Some(1));
        assert_eq!(buf[0], 0xbb, "last region with same start must win");
    }

    /// An empty lookup table always returns `None`.
    #[test]
    fn lookup_table_empty_returns_none() {
        let table = MemRegionsLookupTable::new([]);
        let mut buf = [0u8; 1];
        assert_eq!(table.read(0x1000, &mut buf), None);
    }

    /// The gap between two adjacent non-overlapping regions is not mapped.
    #[test]
    fn lookup_table_gap_between_regions_is_none() {
        let table = MemRegionsLookupTable::new([
            make_region(0x1000, 8),  // 0x1000..0x1008
            make_region(0x1010, 8),  // 0x1010..0x1018
        ]);
        let mut buf = [0u8; 1];
        // 0x1008..0x1010 is a gap
        assert_eq!(table.read(0x1008, &mut buf), None);
        assert_eq!(table.read(0x100f, &mut buf), None);
    }

    // ── RegionsMemReader ──────────────────────────────────────────────────────

    /// `RegionsMemReader` correctly delegates to the underlying lookup table.
    #[test]
    fn regions_mem_reader_delegates_read() {
        let mut r = make_region(0x4000, 8);
        r.data = vec![10, 20, 30, 40, 50, 60, 70, 80];
        let table = MemRegionsLookupTable::new([r]);
        let reader = RegionsMemReader::new(table);

        let mut buf = [0u8; 3];
        assert_eq!(reader.read(0x4002, &mut buf), Some(3));
        assert_eq!(buf, [30, 40, 50]);
    }

    /// `RegionsMemReader` returns `None` for an unmapped address.
    #[test]
    fn regions_mem_reader_miss_returns_none() {
        let table = MemRegionsLookupTable::new([make_region(0x4000, 8)]);
        let reader = RegionsMemReader::new(table);
        let mut buf = [0u8; 1];
        assert_eq!(reader.read(0x9000, &mut buf), None);
    }
}
