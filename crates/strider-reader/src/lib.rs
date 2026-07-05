//! Memory readers for the Strider binary analysis framework.
//!
//! The crate provides:
//!   * Generic region-based memory storage ([`MemRegion`],
//!     [`MemRegionsLookupTable`]) that any reader backend can compose.
//!   * A re-export of the [`ReadOnlyMemory`] trait (defined in
//!     `strider-ir`) used by the optimizer's `LoadReadOnly` pass to resolve
//!     compile-time-constant loads.
//!   * An ELF backend in the [`elf`] module that implements both
//!     [`rsleigh::MemReader`] (for Sleigh instruction fetch) and
//!     [`ReadOnlyMemory`] from the same underlying regions.
//!
//! New reader backends (raw blobs, PE, Mach-O, …) can live alongside `elf`
//! and implement the same traits so they plug interchangeably into the
//! pipeline.

use std::collections::BTreeMap;

/// Crate-level `Result` alias.  Every fallible function in `strider-reader`
/// returns this type.
pub type Result<T> = anyhow::Result<T>;

pub mod elf;
pub use elf::{ElfFileMemReader, load_elf};

// ── MemReadError ─────────────────────────────────────────────────────────────
//
// rsleigh 4.0.0's [`rsleigh::MemReader`] requires `type Err: std::error::Error
// + 'static`.  Strider's readers want to keep using the ergonomic
// [`anyhow::Error`] for everything else, but `anyhow::Error` itself does *not*
// implement [`std::error::Error`] (precisely so it can hold any error
// transparently).  This thin wrapper bridges the gap: it owns an
// `anyhow::Error`, implements [`std::error::Error`] by delegating
// `Display` / `Debug` / `source` to the wrapped value, and offers `From`
// conversions so call sites can use `?` and `anyhow!`/`bail!` as before.

/// Error type returned by every [`rsleigh::MemReader`] impl in the strider
/// crates.  Wraps an [`anyhow::Error`] so the trait's `std::error::Error`
/// bound (introduced in rsleigh 4.0.0) is satisfied while preserving the
/// `anyhow!` / `?` ergonomics callers already rely on.
#[derive(Debug)]
pub struct MemReadError(pub(crate) anyhow::Error);

impl std::fmt::Display for MemReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.0, f)
    }
}

impl std::error::Error for MemReadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        // `anyhow::Error` does not implement `std::error::Error`, but its
        // inner cause does.  Walk one level down so consumers that
        // chase `source()` see the real underlying error.
        self.0.source()
    }
}

impl From<anyhow::Error> for MemReadError {
    fn from(err: anyhow::Error) -> Self {
        MemReadError(err)
    }
}

// `From<MemReadError> for anyhow::Error` is provided automatically by
// anyhow's blanket `impl<E: std::error::Error + Send + Sync + 'static>
// From<E> for anyhow::Error`, since `MemReadError: std::error::Error +
// Send + Sync + 'static`.

// ── ReadOnlyMemory trait ──────────────────────────────────────────────────────
//
// The trait itself lives in the generic `read-only-memory` crate so the
// optimizer crates can depend on it without back-edging through
// `strider-reader`.  Re-exported here for backwards compatibility; the
// concrete `ElfFileMemReader` impl in `elf.rs` continues to implement
// `read_only_memory::ReadOnlyMemory` under the alias `crate::ReadOnlyMemory`.
pub use read_only_memory::ReadOnlyMemory;

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
    /// Returns an error when `start_addr + data.len()` would exceed
    /// `u64::MAX`. This guarantees that downstream methods
    /// ([`end_addr`](Self::end_addr), [`contains`](Self::contains),
    /// [`read`](Self::read)) can treat the region's end as a plain `u64`.
    pub fn new(start_addr: u64, data: Vec<u8>) -> Result<Self> {
        let len = data.len() as u64;
        start_addr.checked_add(len).ok_or_else(|| {
            anyhow::anyhow!("region at {start_addr:#x} with length {len} would overflow u64")
        })?;
        Ok(Self { start_addr, data })
    }

    /// First virtual address covered by this region.
    pub fn start_addr(&self) -> u64 {
        self.start_addr
    }

    /// Raw bytes of the region, starting at [`start_addr`](Self::start_addr).
    pub fn data(&self) -> &[u8] {
        &self.data
    }

    /// Mutable view of the region's bytes.  The Vec's length is not
    /// resizable through this view (the slice doesn't expose
    /// truncate/extend), so the constructor's "no overflow" invariant
    /// on `start_addr + data.len()` survives.  Used by relocation
    /// appliers to patch in-place without rebuilding the region.
    pub fn data_mut(&mut self) -> &mut [u8] {
        &mut self.data
    }

    /// One past the last virtual address covered by this region.
    ///
    /// `end_addr == start_addr + data.len()`. Cannot overflow: the
    /// constructor [`new`](Self::new) rejects any `(start_addr, data)` pair
    /// that would.
    pub fn end_addr(&self) -> u64 {
        self.start_addr + self.data.len() as u64
    }

    /// Returns `true` when `addr` falls within `[start_addr, end_addr)`.
    pub fn contains(&self, addr: u64) -> bool {
        addr >= self.start_addr && addr < self.end_addr()
    }

    /// Reads bytes starting at `addr` into `out`.
    ///
    /// Returns:
    /// - `Some(n)` when [`contains(addr)`](Self::contains) — `n` is the number
    ///   of bytes copied, with `n <= out.len()`. `n` is less than `out.len()`
    ///   when `addr + out.len()` extends past the end of this region; in
    ///   particular, `n == 0` when `out` is empty (the address is mapped but
    ///   the caller asked for zero bytes).
    /// - `None` when `!contains(addr)` — that is, `addr < start_addr` or
    ///   `addr >= end_addr`. A zero-byte read at exactly `end_addr` returns
    ///   `None` rather than `Some(0)`, mirroring the rule that `end_addr`
    ///   itself is not part of the region. Note that for an empty region
    ///   (`data.len() == 0`), `start_addr == end_addr`, so every address
    ///   satisfies `addr >= end_addr` and reads always return `None`.
    pub fn read(&self, addr: u64, out: &mut [u8]) -> Option<usize> {
        let (offset, available) = self.available_at(addr)?;
        let to_copy = available.min(out.len());
        out[..to_copy].copy_from_slice(&self.data[offset..offset + to_copy]);
        Some(to_copy)
    }

    /// Returns `(offset, available)` for `addr` within this region, where
    /// `offset` is the byte index into [`data`](Self::data) and `available`
    /// is the (non-zero) number of bytes from `addr` to the region's end.
    ///
    /// Returns `None` when `!contains(addr)`.  Lets a caller decide which of
    /// several overlapping regions best satisfies a request before writing.
    fn available_at(&self, addr: u64) -> Option<(usize, usize)> {
        let offset = usize::try_from(addr.checked_sub(self.start_addr)?).ok()?;
        let available = self.data.len().checked_sub(offset)?;
        (available != 0).then_some((offset, available))
    }

    /// Returns `true` when this region fully covers the request
    /// `[addr, addr + len)` — `addr` is mapped and the request doesn't
    /// straddle past [`end_addr`](Self::end_addr).  An `addr + len` that
    /// would overflow `u64` is treated as not covered.
    ///
    /// This is the single source of truth for the "highest-start-down,
    /// must-fully-cover" coverage rule shared by
    /// [`MemRegionsLookupTable::read`]'s full-coverage fast path and the
    /// relocation patcher's covering-region lookup.
    pub fn fully_covers(&self, addr: u64, len: usize) -> bool {
        match addr.checked_add(len as u64) {
            Some(end) => self.contains(addr) && end <= self.end_addr(),
            None => false,
        }
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
        Self {
            regions: regions.into_iter().map(|r| (r.start_addr(), r)).collect(),
        }
    }

    /// Reads bytes starting at `addr` from whichever region contains it.
    ///
    /// Returns `None` when no region contains `addr`.
    /// Partial reads are possible — see [`MemRegion::read`].
    ///
    /// Candidates are walked from highest `start_addr <= addr` downward.  A
    /// region that fully satisfies the request (highest such start address
    /// wins) is used immediately; otherwise the region covering the most of
    /// the request is chosen.  This means a shorter region sitting inside a
    /// larger one shadows the larger one only for the bytes it actually
    /// covers — a read straddling the inner region's end falls through to the
    /// fully-covering outer region rather than returning a short partial read.
    ///
    /// # Overlapping regions with differing bytes
    ///
    /// When two regions overlap at *distinct* start addresses and disagree
    /// on the bytes in the overlap (a malformed / synthesised region set —
    /// well-formed ELF loadable ranges are disjoint), the resolution is
    /// fully specified by the two rules above and is intentionally
    /// **all-or-most**, not a per-byte merge:
    ///
    /// - If exactly one candidate region *fully covers* the request, that
    ///   region's bytes win (the highest-start such region if several do).
    /// - Otherwise the region that covers the **most** of the request wins
    ///   outright, and `out` is filled entirely from that one region.  When
    ///   two candidates tie on coverage length, the highest-start one wins
    ///   (it is encountered first and `best` updates only on strictly
    ///   greater coverage).
    ///
    /// In particular a partial read that *straddles* a shorter higher-start
    /// region's end resolves to the lower-start region that reaches further,
    /// not to the higher-start region's truncated prefix — `out` is never a
    /// cross-region byte mix.
    ///
    /// Availability is computed without writing so `out` is filled exactly
    /// once from the winning region (no cross-region byte mixing).
    pub fn read(&self, addr: u64, out: &mut [u8]) -> Option<usize> {
        let mut best: Option<(&MemRegion, usize)> = None;
        for (_, region) in self.regions.range(..=addr).rev() {
            // A region that covers the whole request wins outright; iterating
            // highest-start-first means the latest-starting such region wins.
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

    /// Fill-all-or-error read: copies the exact mapped bytes into `buf` (no
    /// endianness swap), erroring if `addr` is unmapped or the request straddles
    /// the end of a region (a short fill).  This is the single source of truth
    /// for the `ReadOnlyMemory::read` contract that every region-backed reader
    /// (ELF, Python buffer) needs — `LoadReadOnly` must never fold a constant
    /// from partial bytes.
    ///
    /// # Errors
    ///
    /// Returns an error if `addr` is not mapped by any region, or if the
    /// request straddles the end of a region so fewer than `buf.len()` bytes
    /// are available (a short fill).
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
