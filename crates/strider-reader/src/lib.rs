use std::collections::{BTreeMap, BTreeSet};

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

    /// One `stat` of the file these bytes were mapped from, compared against
    /// what it was at map time. Regions over owned bytes answer `Ok` with no
    /// syscall.
    ///
    /// Belongs at the top of an operation, never inside one: [`read`] does no
    /// syscall and cannot, so a change racing a read in progress is still a
    /// torn read or a SIGBUS past a shortened end.
    ///
    /// # Errors
    ///
    /// When the mapped file no longer stats, or no longer looks like the file
    /// that was mapped.
    ///
    /// [`read`]: Self::read
    pub fn check_unchanged(&self) -> Result<()> {
        self.bytes.check_unchanged()
    }

    /// Which mapping backs this region, for deduping [`check_unchanged`].
    ///
    /// [`check_unchanged`]: Self::check_unchanged
    fn mapping_id(&self) -> Option<usize> {
        self.bytes.mapping_id()
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
/// Regions sharing a start address collapse, last-inserted wins. A read is
/// resolved through a [`RegionIndex`], so it costs O(log n) whether the set is
/// disjoint or nests.
#[derive(Debug)]
pub struct MemRegionsLookupTable {
    /// Ascending by start address, one region per start.
    regions: Vec<MemRegion>,
    index: RegionIndex,
}

/// Ascending-start index over a slice of [`MemRegion`]s: which of them cover a
/// request, answered through a max-end tree instead of a walk down every lower
/// start.
///
/// The indices it yields are into that slice, which must not be reordered
/// while the index is held. Equal-start regions are all indexed, the highest
/// slice index among them first.
#[derive(Debug)]
pub struct RegionIndex {
    /// Ascending by `start`, equal starts in slice order.
    entries: Vec<IndexEntry>,
    /// Each entry's `end_addr`, in the same order.
    ends: MaxEnd,
}

#[derive(Debug)]
struct IndexEntry {
    start: u64,
    /// Greatest `end_addr` of this entry and every lower-start one: nothing at
    /// or below this start reaches past it.
    reach: u64,
    /// Index into the indexed slice.
    index: usize,
}

impl RegionIndex {
    pub fn new(regions: &[MemRegion]) -> Self {
        let mut order: Vec<usize> = (0..regions.len()).collect();
        // Stable, so equal starts keep slice order.
        order.sort_by_key(|&i| regions[i].start_addr());
        let mut reach = 0u64;
        let entries: Vec<IndexEntry> = order
            .iter()
            .map(|&index| {
                reach = reach.max(regions[index].end_addr());
                IndexEntry {
                    start: regions[index].start_addr(),
                    reach,
                    index,
                }
            })
            .collect();
        let ends: Vec<u64> = order.iter().map(|&i| regions[i].end_addr()).collect();
        Self {
            entries,
            ends: MaxEnd::new(&ends),
        }
    }

    /// Slice indices of every region fully covering `[addr, addr + len)`,
    /// highest `start` first; a `len` of 0 asks only that `addr` be mapped.
    ///
    /// One O(log n) descent per index yielded, however deeply the image nests.
    /// A read a region fully covers costs one descent. A partial read costs
    /// two: the miss here, then the widest-serving fallback.
    pub fn covering(&self, addr: u64, len: u64) -> Covering<'_> {
        // Without the floor a region ENDING at `addr` would answer a
        // zero-length request, which it does not contain.
        self.walk(
            self.entries.partition_point(|e| e.start <= addr),
            addr.checked_add(len.max(1)),
        )
    }

    /// Slice indices of every region overlapping `[lo, hi)`, highest `start`
    /// first; none at all for an empty range.
    pub fn overlapping(&self, lo: u64, hi: u64) -> Covering<'_> {
        self.walk(
            self.entries.partition_point(|e| e.start < hi),
            (lo < hi).then_some(lo + 1),
        )
    }

    /// Slice index of the region serving the most bytes from `addr`, ties
    /// going to the highest `start`; `None` when nothing maps `addr`.
    pub(crate) fn widest_at(&self, addr: u64) -> Option<usize> {
        let hi = self
            .entries
            .partition_point(|e| e.start <= addr)
            .checked_sub(1)?;
        // Bytes served grow with the end address, so the widest region is the
        // one attaining the prefix maximum, and the rightmost such entry is
        // the highest start among ties.
        let reach = self.entries[hi].reach;
        if reach <= addr {
            return None;
        }
        Some(self.entries[self.ends.rightmost(hi, reach)?].index)
    }

    fn walk(&self, upper: usize, want: Option<u64>) -> Covering<'_> {
        match want {
            Some(want) => Covering {
                index: self,
                upper,
                want,
            },
            // An end past `u64::MAX`, or an empty range.
            None => Covering {
                index: self,
                upper: 0,
                want: 0,
            },
        }
    }
}

/// The regions a [`RegionIndex`] query matched, highest `start` first.
pub struct Covering<'a> {
    index: &'a RegionIndex,
    /// One past the highest entry left to consider.
    upper: usize,
    /// The end address a match has to reach.
    want: u64,
}

impl Iterator for Covering<'_> {
    type Item = usize;

    fn next(&mut self) -> Option<usize> {
        let slot = self
            .index
            .ends
            .rightmost(self.upper.checked_sub(1)?, self.want)?;
        self.upper = slot;
        Some(self.index.entries[slot].index)
    }
}

/// Max-end segment tree over `ends`, which is in ascending-start order.
///
/// Answers, in O(log n): the last entry at or below a given index whose end
/// reaches `want`.
#[derive(Debug)]
struct MaxEnd {
    /// `1`-rooted, leaves at `size..size * 2`.
    tree: Vec<u64>,
    size: usize,
}

impl MaxEnd {
    fn new(ends: &[u64]) -> Self {
        if ends.is_empty() {
            return Self {
                tree: Vec::new(),
                size: 0,
            };
        }
        let size = ends.len().next_power_of_two();
        let mut tree = vec![0u64; size * 2];
        tree[size..size + ends.len()].copy_from_slice(ends);
        for i in (1..size).rev() {
            tree[i] = tree[i * 2].max(tree[i * 2 + 1]);
        }
        Self { tree, size }
    }

    /// Rightmost index in `[0, hi]` whose end is at least `want`.
    fn rightmost(&self, hi: usize, want: u64) -> Option<usize> {
        if self.size == 0 {
            return None;
        }
        self.descend(1, 0, self.size - 1, hi, want)
    }

    fn descend(
        &self,
        node: usize,
        lo: usize,
        hi_node: usize,
        hi: usize,
        want: u64,
    ) -> Option<usize> {
        if lo > hi || self.tree[node] < want {
            return None;
        }
        if lo == hi_node {
            return Some(lo);
        }
        let mid = lo + (hi_node - lo) / 2;
        // Right first: the answer is the rightmost index, so the first hit wins.
        self.descend(node * 2 + 1, mid + 1, hi_node, hi, want)
            .or_else(|| self.descend(node * 2, lo, mid, hi, want))
    }
}

impl MemRegionsLookupTable {
    /// Two regions sharing a start address collapse to the later one.
    pub fn new<I: IntoIterator<Item = MemRegion>>(regions: I) -> Self {
        let regions: Vec<MemRegion> = regions
            .into_iter()
            .map(|r| (r.start_addr(), r))
            .collect::<BTreeMap<u64, MemRegion>>()
            .into_values()
            .collect();
        let index = RegionIndex::new(&regions);
        Self { regions, index }
    }

    /// [`MemRegion::check_unchanged`] over the table, one `stat` per distinct
    /// mapping rather than per region: an image's regions all share one.
    ///
    /// # Errors
    ///
    /// When any mapped file behind the table changed since it was mapped.
    pub fn check_unchanged(&self) -> Result<()> {
        let mut stat_ed: BTreeSet<usize> = BTreeSet::new();
        for region in &self.regions {
            let Some(id) = region.mapping_id() else {
                continue;
            };
            if stat_ed.insert(id) {
                region.check_unchanged()?;
            }
        }
        Ok(())
    }

    /// Reads bytes at `addr` from whichever region wins; `None` when none
    /// contains `addr`. Partial reads are possible, see [`MemRegion::read`].
    ///
    /// Resolution is **all-or-most**, never a per-byte merge: `out` is filled
    /// from exactly one region, so it is never a cross-region byte mix. A
    /// region fully covering the request wins outright (highest start among
    /// those); otherwise the region covering the most of it wins, ties going to
    /// the highest start.
    ///
    /// A read straddling a shorter inner region's end therefore falls through
    /// to the fully-covering outer region rather than returning the inner
    /// region's truncated prefix.
    pub fn read(&self, addr: u64, out: &mut [u8]) -> Option<usize> {
        let winner = self
            .index
            .covering(addr, out.len() as u64)
            .next()
            .or_else(|| self.index.widest_at(addr))?;
        self.regions[winner].read(addr, out)
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

#[cfg(test)]
mod tests {
    use super::{MemRegion, RegionIndex};

    /// The fallback a read takes when no region fully covers it, and the one
    /// method here with no caller outside the crate.
    #[test]
    fn widest_at_picks_the_region_serving_the_most() {
        let regions = [
            MemRegion::new(0x1000, vec![0u8; 0x40]).unwrap(),
            MemRegion::new(0x1010, vec![0u8; 0x10]).unwrap(),
        ];
        let index = RegionIndex::new(&regions);
        assert_eq!(
            index.widest_at(0x1010),
            Some(0),
            "the higher start serves fewer bytes from here"
        );
        assert_eq!(index.widest_at(0x1030), Some(0));
        assert_eq!(index.widest_at(0x0fff), None);
        assert_eq!(index.widest_at(0x1040), None);
    }
}
