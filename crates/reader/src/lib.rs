#![cfg_attr(
    test,
    allow(
        clippy::panic,
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::unreachable
    )
)]

//! Memory readers for the Strider binary analysis framework.
//!
//! The crate provides:
//!   * Generic region-based memory storage ([`MemRegion`],
//!     [`MemRegionsLookupTable`], [`RegionsMemReader`]) that any reader
//!     backend can compose.
//!   * The [`ReadOnlyMemory`] trait used by the optimizer's `LoadReadOnly`
//!     pass to resolve compile-time-constant loads.
//!   * An ELF backend in the [`elf`] module that implements both
//!     [`rsleigh::MemReader`] (for Sleigh instruction fetch) and
//!     [`ReadOnlyMemory`] from the same underlying regions.
//!
//! New reader backends (raw blobs, PE, Mach-O, …) can live alongside `elf`
//! and implement the same traits so they plug interchangeably into the
//! pipeline.

use std::collections::BTreeMap;

pub mod error;
pub use error::{Error, ErrorKind, Result};

pub mod elf;
pub use elf::{ElfFileMemReader, load_elf};

// ── ReadOnlyMemory trait ──────────────────────────────────────────────────────

/// Provides read access to a statically-known region of memory (e.g. a
/// binary's `.rodata` or `.text` section).
///
/// The optimizer uses this trait to resolve `Load` nodes whose address is a
/// compile-time constant into the corresponding constant values, eliminating
/// the load entirely.
pub trait ReadOnlyMemory: Send + Sync {
    /// Returns the value at `addr` in `space` as an unsigned integer of `size`
    /// bytes, or `None` if the address is not part of read-only memory or the
    /// read cannot be satisfied.
    fn read(&self, space: rsleigh::VnSpace, addr: u64, size: usize) -> Option<u64>;
}

// ── MemRegion ─────────────────────────────────────────────────────────────────

/// A contiguous range of bytes loaded at a fixed virtual address.
///
/// Corresponds to one backend-specific mapping (e.g. an ELF section or an
/// entry from a raw blob manifest) into the virtual address space of the
/// target binary.
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
/// with a concrete backend (like `ElfFileMemReader`) directly.
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
        let table = MemRegionsLookupTable::new([make_region(0x1000, 16), make_region(0x2000, 16)]);
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
            make_region(0x1000, 8), // 0x1000..0x1008
            make_region(0x1010, 8), // 0x1010..0x1018
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
