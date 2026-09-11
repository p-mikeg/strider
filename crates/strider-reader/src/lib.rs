use std::collections::BTreeMap;

pub type Result<T> = anyhow::Result<T>;

mod bytes;
pub mod elf;
pub(crate) use bytes::FileBytes;
pub use elf::{ElfFileMemReader, OwnedElf, load_elf};

/// Error type for every [`rsleigh::MemReader`] impl in the strider crates.
///
/// `rsleigh::MemReader` requires `Err: std::error::Error + 'static`, which
/// `anyhow::Error` does not implement. This wrapper satisfies the
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
        self.0.source()
    }
}

impl From<anyhow::Error> for MemReadError {
    fn from(err: anyhow::Error) -> Self {
        MemReadError(err)
    }
}

pub use read_only_memory::ReadOnlyMemory;

/// A relocation site's patched value, applied over the file-initial bytes when
/// a read crosses it.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Patch {
    addr: u64,
    len: u8,
    /// `len` target-endian bytes of the field value.
    value: [u8; 8],
}

/// Widest field any [`Patch`] covers, so a read's first candidate patch is the
/// first one starting at or after `addr - (MAX_PATCH_LEN - 1)`.
const MAX_PATCH_LEN: u64 = 8;

impl Patch {
    /// The low `size_bytes` of `value` at `addr`, in the target's endianness.
    ///
    /// # Preconditions
    ///
    /// `size_bytes <= 8`, since `value` is a `u64`. Every relocation kind that
    /// reaches here picks a size in `{1, 2, 4, 8}`.
    pub(crate) fn new(addr: u64, value: u64, size_bytes: usize, endian_le: bool) -> Option<Self> {
        // No-op in release rather than an opaque slice panic.
        if size_bytes > 8 {
            debug_assert!(
                false,
                "Patch::new: size_bytes={size_bytes} exceeds u64 width; every ELF \
                 relocation kind must select size_bytes in {{1, 2, 4, 8}}"
            );
            return None;
        }
        let mut bytes = [0u8; 8];
        // Truncation to the field width; signedness is irrelevant for
        // fixed-width 2's-complement bit patterns.
        if endian_le {
            bytes[..size_bytes].copy_from_slice(&value.to_le_bytes()[..size_bytes]);
        } else {
            // Low N bytes, most-significant first.
            bytes[..size_bytes].copy_from_slice(&value.to_be_bytes()[8 - size_bytes..]);
        }
        Some(Self {
            addr,
            len: size_bytes as u8,
            value: bytes,
        })
    }

    fn end(&self) -> u64 {
        self.addr + u64::from(self.len)
    }
}

/// A contiguous range of bytes loaded at a fixed virtual address: one mapping
/// (ELF segment or section) into the target's address space.
///
/// The bytes are a window into a shared immutable buffer, and relocations are
/// a sorted patch list applied to the caller's buffer on read, so loading an
/// image neither copies it nor faults in the pages nothing reads.
#[derive(Clone)]
pub struct MemRegion {
    start_addr: u64,
    bytes: FileBytes,
    offset: usize,
    len: usize,
    /// Sorted by `addr`, insertion order kept within one address so the
    /// last-collected patch at a site is the one that lands.
    patches: Option<std::sync::Arc<[Patch]>>,
}

impl std::fmt::Debug for MemRegion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MemRegion")
            .field("start_addr", &format_args!("{:#x}", self.start_addr))
            .field("len", &self.len)
            .field("patches", &self.patches.as_ref().map_or(0, |p| p.len()))
            .finish()
    }
}

impl MemRegion {
    /// # Errors
    ///
    /// Errors when `start_addr + data.len()` would exceed `u64::MAX`.
    pub fn new(start_addr: u64, data: Vec<u8>) -> Result<Self> {
        let len = data.len();
        Self::check_end(start_addr, len)?;
        Ok(Self {
            start_addr,
            bytes: FileBytes::from_vec(data),
            offset: 0,
            len,
            patches: None,
        })
    }

    /// `[file_offset, file_offset + len)` of `bytes` mapped at `start_addr`,
    /// sharing the buffer rather than copying out of it.
    ///
    /// # Errors
    ///
    /// When the window runs past the end of `bytes`, or `start_addr + len`
    /// would exceed `u64::MAX`.
    pub(crate) fn window(
        start_addr: u64,
        bytes: &FileBytes,
        file_offset: u64,
        len: u64,
    ) -> Result<Self> {
        let (offset, len) = (usize::try_from(file_offset)?, usize::try_from(len)?);
        if offset.checked_add(len).is_none_or(|end| end > bytes.len()) {
            anyhow::bail!(
                "file window [{offset}, +{len}) runs past the {} byte image",
                bytes.len()
            );
        }
        Self::check_end(start_addr, len)?;
        Ok(Self {
            start_addr,
            bytes: bytes.clone(),
            offset,
            len,
            patches: None,
        })
    }

    fn check_end(start_addr: u64, len: usize) -> Result<()> {
        start_addr.checked_add(len as u64).ok_or_else(|| {
            anyhow::anyhow!("region at {start_addr:#x} with length {len} would overflow u64")
        })?;
        Ok(())
    }

    /// Replaces the patch list, sorting by address; see the field docs for the
    /// equal-address rule.
    pub(crate) fn set_patches(&mut self, mut patches: Vec<Patch>) {
        if patches.is_empty() {
            self.patches = None;
            return;
        }
        // Stable, so equal-address patches keep collection order.
        patches.sort_by_key(|p| p.addr);
        self.patches = Some(patches.into());
    }

    pub fn start_addr(&self) -> u64 {
        self.start_addr
    }

    /// The file-initial bytes, with no relocation patch applied.
    pub(crate) fn raw(&self) -> &[u8] {
        &self.bytes.as_slice()[self.offset..self.offset + self.len]
    }

    /// One past the last address covered. Cannot overflow: the constructors
    /// reject any pair that would.
    pub fn end_addr(&self) -> u64 {
        self.start_addr + self.len as u64
    }

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
        out[..to_copy].copy_from_slice(&self.raw()[offset..offset + to_copy]);
        self.apply_patches(addr, &mut out[..to_copy]);
        Some(to_copy)
    }

    /// Overwrites the parts of `buf` (holding the bytes at `addr`) that a
    /// relocation patch covers.
    fn apply_patches(&self, addr: u64, buf: &mut [u8]) {
        let Some(patches) = self.patches.as_ref() else {
            return;
        };
        let end = addr.saturating_add(buf.len() as u64);
        let first = patches.partition_point(|p| p.addr < addr.saturating_sub(MAX_PATCH_LEN - 1));
        for p in &patches[first..] {
            if p.addr >= end {
                break;
            }
            let (lo, hi) = (p.addr.max(addr), p.end().min(end));
            if lo >= hi {
                continue;
            }
            let (dst, src, n) = (
                (lo - addr) as usize,
                (lo - p.addr) as usize,
                (hi - lo) as usize,
            );
            buf[dst..dst + n].copy_from_slice(&p.value[src..src + n]);
        }
    }

    /// `(index into the window, non-zero bytes remaining)`, or `None` when
    /// `addr` is outside.
    fn available_at(&self, addr: u64) -> Option<(usize, usize)> {
        let offset = usize::try_from(addr.checked_sub(self.start_addr)?).ok()?;
        let available = self.len.checked_sub(offset)?;
        (available != 0).then_some((offset, available))
    }

    /// This region covers all of `[addr, addr + len)`. An `addr + len` that
    /// overflows `u64` counts as not covered.
    pub(crate) fn fully_covers(&self, addr: u64, len: usize) -> bool {
        match addr.checked_add(len as u64) {
            Some(end) => self.contains(addr) && end <= self.end_addr(),
            None => false,
        }
    }

    /// Both regions serve the same bytes across `[lo, hi)`, patches included.
    /// Any part of the range either region fails to serve in full, whether
    /// unmapped or short of the request, counts as differing.
    pub fn same_bytes_in(&self, other: &MemRegion, lo: u64, hi: u64) -> bool {
        let mut addr = lo;
        let (mut a, mut b) = ([0u8; 4096], [0u8; 4096]);
        while addr < hi {
            let want = (hi - addr).min(a.len() as u64) as usize;
            let (Some(n), Some(m)) = (
                self.read(addr, &mut a[..want]),
                other.read(addr, &mut b[..want]),
            ) else {
                return false;
            };
            if (n, m) != (want, want) || a[..want] != b[..want] {
                return false;
            }
            addr += want as u64;
        }
        true
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
    /// Each region with the greatest `end_addr` among it and everything
    /// starting at or below it, so a descending walk can stop once no earlier
    /// region can still reach the address.
    regions: BTreeMap<u64, (MemRegion, u64)>,
}

impl MemRegionsLookupTable {
    /// Two regions sharing a start address collapse to the later one.
    pub fn new<I: IntoIterator<Item = MemRegion>>(regions: I) -> Self {
        let mut regions: BTreeMap<u64, (MemRegion, u64)> = regions
            .into_iter()
            .map(|r| {
                let end = r.end_addr();
                (r.start_addr(), (r, end))
            })
            .collect();
        // Prefix maximum in ascending start order.
        let mut running = 0u64;
        for (_, reach) in regions.values_mut() {
            running = running.max(*reach);
            *reach = running;
        }
        Self { regions }
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
    /// A read straddling a shorter inner region's end therefore falls through
    /// to the fully-covering outer region rather than returning the inner
    /// region's truncated prefix.
    pub fn read(&self, addr: u64, out: &mut [u8]) -> Option<usize> {
        let mut best: Option<(&MemRegion, usize)> = None;
        for (_, (region, reach)) in self.regions.range(..=addr).rev() {
            // Nothing at or below this start reaches `addr`, so neither will
            // anything further down. Without this an UNMAPPED read scans every
            // region with a lower start, which is O(n) per read.
            if *reach <= addr {
                break;
            }
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
    /// A short fill errors rather than truncating.
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
