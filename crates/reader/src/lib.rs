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
///
/// Fields are private so the "no overflow" invariant established by
/// [`new`](Self::new) cannot be bypassed after construction. Read access
/// is via [`start_addr`](Self::start_addr) and [`data`](Self::data).
#[derive(Clone, Debug)]
pub struct MemRegion {
    start_addr: u64,
    data: Vec<u8>,
}

impl MemRegion {
    /// Creates a new `MemRegion` loaded at `start_addr`.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorKind::RegionOverflow`](error::ErrorKind::RegionOverflow)
    /// when `start_addr + data.len()` would exceed `u64::MAX`. This guarantees
    /// that downstream methods ([`end_addr`](Self::end_addr),
    /// [`contains`](Self::contains), [`read`](Self::read)) can treat the
    /// region's end as a plain `u64`.
    pub fn new(start_addr: u64, data: Vec<u8>) -> Result<Self> {
        let len = data.len() as u64;
        if start_addr.checked_add(len).is_none() {
            return Err(error::ErrorKind::RegionOverflow { start_addr, len }.into());
        }
        Ok(Self { start_addr, data })
    }

    /// First virtual address covered by this region.
    #[must_use]
    pub fn start_addr(&self) -> u64 {
        self.start_addr
    }

    /// Raw bytes of the region, starting at [`start_addr`](Self::start_addr).
    #[must_use]
    pub fn data(&self) -> &[u8] {
        &self.data
    }

    /// One past the last virtual address covered by this region.
    ///
    /// `end_addr == start_addr + data.len()`. Cannot overflow: the
    /// constructor [`new`](Self::new) rejects any `(start_addr, data)` pair
    /// that would.
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
        let offset = usize::try_from(addr.checked_sub(self.start_addr)?).ok()?;
        let available = self.data.len().checked_sub(offset)?;
        if available == 0 {
            return None;
        }
        let to_copy = available.min(out.len());
        out[..to_copy].copy_from_slice(&self.data[offset..offset + to_copy]);
        Some(to_copy)
    }
}

// ── MemRegionsLookupTable ─────────────────────────────────────────────────────

/// A fast lookup table over a collection of [`MemRegion`]s, possibly overlapping.
///
/// Regions are indexed by start address in a `BTreeMap`, giving O(log n)
/// candidate lookup via a range query. Two regions sharing the same start
/// address collapse: the last-inserted one wins. When regions overlap at
/// different start addresses, reads resolve by walking candidates from the
/// highest `start_addr <= addr` downward and returning the first region that
/// contains `addr`; this is O(log n) in the usual non-overlapping case and
/// O(n) in the worst case where every earlier region must be consulted.
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
            map.insert(region.start_addr(), region);
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

