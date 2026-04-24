//! Memory readers for the Strider binary analysis framework.
//!
//! The crate provides:
//!   * Generic region-based memory storage ([`MemRegion`],
//!     [`MemRegionsLookupTable`]) that any reader backend can compose.
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
    ///
    /// Candidates are walked from highest `start_addr <= addr` downward: the
    /// usual no-overlap case returns on the first candidate, but if a later,
    /// shorter region sits inside an earlier one the outer region is consulted
    /// for addresses past the inner region's end.
    pub fn read(&self, addr: u64, out: &mut [u8]) -> Option<usize> {
        for (_, region) in self.regions.range(..=addr).rev() {
            if let Some(n) = region.read(addr, out) {
                return Some(n);
            }
        }
        None
    }
}

