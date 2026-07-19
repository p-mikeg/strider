//! Memory readers: generic region storage ([`MemRegion`],
//! [`MemRegionsLookupTable`]) plus an [`elf`] backend that serves both
//! [`rsleigh::MemReader`] (instruction fetch) and [`ReadOnlyMemory`]
//! (constant-load folding) from the same regions.

use std::collections::BTreeMap;

pub type Result<T> = anyhow::Result<T>;

pub mod elf;
pub use elf::{ElfFileMemReader, OwnedElf, load_elf};

/// Error type for every [`rsleigh::MemReader`] impl in the strider crates.
///
/// `rsleigh::MemReader` requires `Err: std::error::Error + 'static`, which
/// `anyhow::Error` deliberately does not implement. This wrapper satisfies the
/// bound while keeping `anyhow!` / `?` usable at call sites.
#[derive(Debug)]
pub struct MemReadError(pub(crate) anyhow::Error);

impl std::fmt::Display for MemReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.0, f)
    }
}

impl std::error::Error for MemReadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        // `anyhow::Error` isn't a `std::error::Error`, but its inner cause is.
        // Walk one level down so `source()` chasers see the real error.
        self.0.source()
    }
}

impl From<anyhow::Error> for MemReadError {
    fn from(err: anyhow::Error) -> Self {
        MemReadError(err)
    }
}

// `From<MemReadError> for anyhow::Error` comes free from anyhow's blanket impl
// over `std::error::Error + Send + Sync + 'static`.

// The trait lives in the generic `read-only-memory` crate so the optimizer
// crates can depend on it without back-edging through `strider-reader`.
pub use read_only_memory::ReadOnlyMemory;

/// A contiguous range of bytes loaded at a fixed virtual address: one
/// backend-specific mapping (ELF section, blob manifest entry, ...) into the
/// target's address space.
///
/// Fields are private so the no-overflow invariant [`new`](Self::new)
/// establishes cannot be bypassed after construction.
#[derive(Clone, Debug)]
pub struct MemRegion {
    start_addr: u64,
    data: Vec<u8>,
}

impl MemRegion {
    /// # Errors
    ///
    /// Errors when `start_addr + data.len()` would exceed `u64::MAX`. This is
    /// what lets [`end_addr`](Self::end_addr), [`contains`](Self::contains) and
    /// [`read`](Self::read) treat the region's end as a plain `u64`.
    pub fn new(start_addr: u64, data: Vec<u8>) -> Result<Self> {
        let len = data.len() as u64;
        start_addr.checked_add(len).ok_or_else(|| {
            anyhow::anyhow!("region at {start_addr:#x} with length {len} would overflow u64")
        })?;
        Ok(Self { start_addr, data })
    }

    pub fn start_addr(&self) -> u64 {
        self.start_addr
    }

    pub fn data(&self) -> &[u8] {
        &self.data
    }

    /// A slice, so length can't change through it and the constructor's
    /// no-overflow invariant survives. Relocation appliers patch through this.
    pub fn data_mut(&mut self) -> &mut [u8] {
        &mut self.data
    }

    /// One past the last address covered. Cannot overflow: [`new`](Self::new)
    /// rejects any pair that would.
    pub fn end_addr(&self) -> u64 {
        self.start_addr + self.data.len() as u64
    }

    /// `addr` falls within `[start_addr, end_addr)`.
    pub fn contains(&self, addr: u64) -> bool {
        addr >= self.start_addr && addr < self.end_addr()
    }

    /// Reads bytes at `addr` into `out`, possibly partially.
    ///
    /// `Some(n)` when [`contains(addr)`](Self::contains); `n < out.len()` when
    /// the request runs past the region's end. `None` otherwise, including a
    /// zero-length read at exactly `end_addr` (the end is exclusive even for a
    /// zero-byte request). An empty region has `start_addr == end_addr`, so it
    /// contains nothing and always returns `None`.
    pub fn read(&self, addr: u64, out: &mut [u8]) -> Option<usize> {
        let (offset, available) = self.available_at(addr)?;
        let to_copy = available.min(out.len());
        out[..to_copy].copy_from_slice(&self.data[offset..offset + to_copy]);
        Some(to_copy)
    }

    /// `(index into data, non-zero bytes remaining)`, or `None` when `addr` is
    /// outside. Lets a caller pick among overlapping regions before writing.
    fn available_at(&self, addr: u64) -> Option<(usize, usize)> {
        let offset = usize::try_from(addr.checked_sub(self.start_addr)?).ok()?;
        let available = self.data.len().checked_sub(offset)?;
        (available != 0).then_some((offset, available))
    }

    /// This region covers all of `[addr, addr + len)`. An `addr + len` that
    /// overflows `u64` counts as not covered.
    ///
    /// Single source of truth for the must-fully-cover rule shared by
    /// [`MemRegionsLookupTable::read`]'s fast path and the relocation patcher's
    /// covering-region lookup.
    pub fn fully_covers(&self, addr: u64, len: usize) -> bool {
        match addr.checked_add(len as u64) {
            Some(end) => self.contains(addr) && end <= self.end_addr(),
            None => false,
        }
    }
}

/// Lookup table over a set of possibly-overlapping [`MemRegion`]s.
///
/// Keyed by start address, so candidate lookup is an O(log n) range query.
/// Regions sharing a start address collapse, last-inserted wins. Overlapping
/// regions at distinct starts resolve by walking candidates from the highest
/// `start_addr <= addr` downward: O(log n) on the usual disjoint set, O(n)
/// worst case.
#[derive(Debug)]
pub struct MemRegionsLookupTable {
    regions: BTreeMap<u64, MemRegion>,
}

impl MemRegionsLookupTable {
    /// Two regions sharing a start address collapse to the later one.
    pub fn new<I: IntoIterator<Item = MemRegion>>(regions: I) -> Self {
        Self {
            regions: regions.into_iter().map(|r| (r.start_addr(), r)).collect(),
        }
    }

    /// Reads bytes at `addr` from whichever region wins; `None` when none
    /// contains `addr`. Partial reads are possible, see [`MemRegion::read`].
    ///
    /// Resolution is **all-or-most**, never a per-byte merge: `out` is filled
    /// from exactly one region, so it is never a cross-region byte mix.
    /// Candidates are walked from the highest `start_addr <= addr` downward.
    /// A region fully covering the request wins outright (highest start among
    /// those); otherwise the region covering the most of it wins, ties going to
    /// the highest start.
    ///
    /// Consequence worth knowing: a read straddling a shorter inner region's
    /// end falls through to the fully-covering outer region rather than
    /// returning the inner region's truncated prefix. Overlapping regions that
    /// disagree on bytes only arise from a malformed or synthesised region set
    /// (well-formed ELF loadable ranges are disjoint), but the rule above
    /// specifies that case rather than leaving it to iteration order.
    pub fn read(&self, addr: u64, out: &mut [u8]) -> Option<usize> {
        let mut best: Option<(&MemRegion, usize)> = None;
        for (_, region) in self.regions.range(..=addr).rev() {
            if region.fully_covers(addr, out.len()) {
                return region.read(addr, out);
            }
            let Some((_, available)) = region.available_at(addr) else {
                continue;
            };
            let n = available.min(out.len());
            if best.is_none_or(|(_, best_n)| n > best_n) {
                best = Some((region, n));
            }
        }
        best.and_then(|(region, _)| region.read(addr, out))
    }

    /// Fill-all-or-error read: copies the mapped bytes into `buf` **raw**, with
    /// no endianness swap. Callers wanting an integer decode them themselves.
    ///
    /// Single source of truth for the `ReadOnlyMemory::read` contract every
    /// region-backed reader (ELF, Python buffer) implements. Short fills must
    /// error, not truncate, so `LoadReadOnly` can never fold a constant out of
    /// partial bytes.
    ///
    /// # Errors
    ///
    /// When `addr` is unmapped, or the request straddles a region's end so
    /// fewer than `buf.len()` bytes are available.
    pub fn read_exact(&self, addr: u64, buf: &mut [u8]) -> anyhow::Result<()> {
        let want = buf.len();
        let got = self
            .read(addr, buf)
            .ok_or_else(|| anyhow::anyhow!("address {addr:#x} is not mapped"))?;
        if got != want {
            anyhow::bail!("read at {addr:#x} spans past mapped memory: got {got} of {want} bytes");
        }
        Ok(())
    }
}
