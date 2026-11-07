use std::collections::BTreeMap;
use std::ops::Range;
use std::sync::Arc;

use crate::{FileBytes, Result, bytes};

/// A relocation site's patched value, applied when a read crosses it, or an
/// uncomputed field (a hole), which serves no bytes.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Patch {
    pub(crate) addr: u64,
    len: u8,
    /// `len` target-endian bytes of the field value.
    value: [u8; 8],
    /// `r_type` of a relocation with no computed value: the field is a hole.
    unmodelled: Option<u32>,
}

/// Widest field any [`Patch`] covers, so a read's first candidate patch is the
/// first one starting at or after `addr - (MAX_PATCH_LEN - 1)`.
pub(crate) const MAX_PATCH_LEN: u64 = 8;

impl Patch {
    /// The low `size_bytes` of `value` at `addr`, in the target's endianness.
    ///
    /// # Panics
    ///
    /// When `size_bytes > 8`: every computed field is 1 to 8 bytes; a wider
    /// one is only ever a hole.
    pub(crate) fn new(addr: u64, value: u64, size_bytes: usize, endian_le: bool) -> Self {
        assert!(size_bytes <= 8, "a {size_bytes}-byte relocation field");
        let mut bytes = [0u8; 8];
        bytes::write_uint(&mut bytes[..size_bytes], value, endian_le);
        Self {
            addr,
            len: size_bytes as u8,
            value: bytes,
            unmodelled: None,
        }
    }

    /// `size_bytes` at `addr` that no read may serve: a relocation of type
    /// `r_type` lands there and its value was not computed.
    pub(crate) fn hole(addr: u64, size_bytes: usize, r_type: u32) -> Self {
        debug_assert!(
            size_bytes as u64 <= MAX_PATCH_LEN,
            "a {size_bytes}-byte hole"
        );
        Self {
            addr,
            len: size_bytes as u8,
            value: [0; 8],
            unmodelled: Some(r_type),
        }
    }

    fn end(&self) -> u64 {
        self.addr + u64::from(self.len)
    }
}

/// One mapping at a fixed virtual address: a window into a shared immutable
/// buffer, with relocations as a sorted patch list applied on read.
#[derive(Clone)]
pub struct MemRegion {
    start_addr: u64,
    bytes: FileBytes,
    offset: usize,
    len: usize,
    /// The image's relocation patches, sorted by `addr` (at one site the last
    /// recorded lands, and a hole beats any value), and the range of them
    /// that can overlap this region.
    patches: Option<(Arc<[Patch]>, Range<usize>)>,
}

impl std::fmt::Debug for MemRegion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MemRegion")
            .field("start_addr", &format_args!("{:#x}", self.start_addr))
            .field("len", &self.len)
            .field("patch_window", &self.patches().len())
            .finish()
    }
}

impl MemRegion {
    /// # Errors
    ///
    /// When `start_addr + data.len()` would exceed `u64::MAX`.
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
    /// sharing the buffer. Errs when the window runs past `bytes` or
    /// `start_addr + len` overflows.
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

    /// The exclusive end must fit a `u64`, so the byte at `u64::MAX` is
    /// unmappable.
    fn check_end(start_addr: u64, len: usize) -> Result<()> {
        start_addr.checked_add(len as u64).ok_or_else(|| {
            anyhow::anyhow!("region at {start_addr:#x} with length {len} would overflow u64")
        })?;
        Ok(())
    }

    /// Shares `all`, the image's patches sorted by `addr`, keeping the range
    /// that can overlap this region; replaces any it carried.
    pub(crate) fn attach_patches(&mut self, all: &Arc<[Patch]>) {
        let lo =
            all.partition_point(|p| p.addr < self.start_addr.saturating_sub(MAX_PATCH_LEN - 1));
        let hi = all.partition_point(|p| p.addr < self.end_addr());
        self.patches = (lo < hi).then(|| (Arc::clone(all), lo..hi));
    }

    /// The patches that can overlap this region, sorted by `addr`.
    fn patches(&self) -> &[Patch] {
        match &self.patches {
            Some((all, range)) => &all[range.clone()],
            None => &[],
        }
    }

    /// How many patches can overlap this region.
    #[cfg(feature = "test-util")]
    pub fn patch_count(&self) -> usize {
        self.patches().len()
    }

    pub fn start_addr(&self) -> u64 {
        self.start_addr
    }

    /// Re-stats the mapped file against map time; `Ok` over owned bytes. A
    /// change racing a [`read`](Self::read) still tears or SIGBUSes.
    ///
    /// # Errors
    ///
    /// When the file no longer stats, or its size or mtime changed.
    pub fn check_unchanged(&self) -> Result<()> {
        self.bytes.check_unchanged()
    }

    /// Which mapping backs this region; `None` for owned bytes.
    pub(crate) fn mapping_id(&self) -> Option<usize> {
        self.bytes.mapping_id()
    }

    /// `[lo, hi)` of this region, sharing its bytes and patches.
    ///
    /// # Panics
    ///
    /// When `[lo, hi)` is not a non-empty part of the region.
    pub(crate) fn slice(&self, lo: u64, hi: u64) -> Self {
        assert!(self.start_addr <= lo && lo < hi && hi <= self.end_addr());
        Self {
            start_addr: lo,
            bytes: self.bytes.clone(),
            offset: self.offset + (lo - self.start_addr) as usize,
            len: (hi - lo) as usize,
            patches: self.patches.clone(),
        }
    }

    /// The file-initial bytes, with no relocation patch applied.
    pub(crate) fn raw(&self) -> &[u8] {
        &self.bytes.as_slice()[self.offset..self.offset + self.len]
    }

    pub fn end_addr(&self) -> u64 {
        self.start_addr + self.len as u64
    }

    pub fn contains(&self, addr: u64) -> bool {
        addr >= self.start_addr && addr < self.end_addr()
    }

    /// Reads bytes at `addr` into `out`: `Some(n)`, short when the request
    /// runs past the end or into a hole, or `None` when `addr` is outside the
    /// region or in a hole.
    pub fn read(&self, addr: u64, out: &mut [u8]) -> Option<usize> {
        let (offset, available) = self.available_at(addr)?;
        let mut to_copy = available.min(out.len());
        let near = self.patches_from(addr);
        if let Some(hole) = first_hole_in(near, addr, to_copy) {
            if hole.addr <= addr {
                return None;
            }
            to_copy = (hole.addr - addr) as usize;
        }
        out[..to_copy].copy_from_slice(&self.raw()[offset..offset + to_copy]);
        apply_patches(near, addr, &mut out[..to_copy]);
        Some(to_copy)
    }

    /// The `r_type` of the uncomputed relocation field covering `addr`, if any.
    pub fn unmodelled_relocation_at(&self, addr: u64) -> Option<u32> {
        self.contains(addr)
            .then(|| first_hole_in(self.patches_from(addr), addr, 1))
            .flatten()
            .filter(|h| h.addr <= addr)
            .and_then(|h| h.unmodelled)
    }

    /// This region's patches from the first that can overlap `addr`.
    fn patches_from(&self, addr: u64) -> &[Patch] {
        let patches = self.patches();
        let first = patches.partition_point(|p| p.addr < addr.saturating_sub(MAX_PATCH_LEN - 1));
        &patches[first..]
    }

    /// `(index into the window, non-zero bytes remaining)`, or `None` when
    /// `addr` is outside.
    fn available_at(&self, addr: u64) -> Option<(usize, usize)> {
        let offset = usize::try_from(addr.checked_sub(self.start_addr)?).ok()?;
        let available = self.len.checked_sub(offset)?;
        (available != 0).then_some((offset, available))
    }

    /// Whether both regions serve the same bytes across `[lo, hi)`, and holes
    /// of the same type; any other unserved byte is a difference.
    pub fn same_bytes_in(&self, other: &MemRegion, lo: u64, hi: u64) -> bool {
        let mut addr = lo;
        let (mut a, mut b) = ([0u8; 4096], [0u8; 4096]);
        while addr < hi {
            let want = (hi - addr).min(a.len() as u64) as usize;
            match (
                self.read(addr, &mut a[..want]),
                other.read(addr, &mut b[..want]),
            ) {
                (Some(n), Some(m)) if n == m && n != 0 && a[..n] == b[..n] => {
                    addr += n as u64;
                }
                (None, None)
                    if self.unmodelled_relocation_at(addr).is_some()
                        && self.unmodelled_relocation_at(addr)
                            == other.unmodelled_relocation_at(addr) =>
                {
                    addr += 1;
                }
                _ => return false,
            }
        }
        true
    }
}

/// The patches of `near` overlapping `[addr, addr + len)`, in address order;
/// `near` starts at the first that can overlap `addr`.
fn overlapping(near: &[Patch], addr: u64, len: usize) -> impl Iterator<Item = &Patch> {
    let end = addr.saturating_add(len as u64);
    near.iter()
        .take_while(move |p| p.addr < end)
        .filter(move |p| p.end() > addr)
}

/// The lowest uncomputed field of `near` overlapping `[addr, addr + len)`.
fn first_hole_in(near: &[Patch], addr: u64, len: usize) -> Option<&Patch> {
    overlapping(near, addr, len).find(|p| p.unmodelled.is_some())
}

/// Overwrites the parts of `buf` (holding the bytes at `addr`) that a patch
/// of `near` covers.
fn apply_patches(near: &[Patch], addr: u64, buf: &mut [u8]) {
    let end = addr.saturating_add(buf.len() as u64);
    for p in overlapping(near, addr, buf.len()) {
        if p.unmodelled.is_some() {
            continue;
        }
        let (lo, hi) = (p.addr.max(addr), p.end().min(end));
        let (dst, src, n) = (
            (lo - addr) as usize,
            (lo - p.addr) as usize,
            (hi - lo) as usize,
        );
        buf[dst..dst + n].copy_from_slice(&p.value[src..src + n]);
    }
}

/// `regions` with each run of equal starts collapsed to its last region.
pub(crate) fn collapse_equal_starts(regions: Vec<MemRegion>) -> Vec<MemRegion> {
    let last: BTreeMap<u64, usize> = regions
        .iter()
        .enumerate()
        .map(|(i, r)| (r.start_addr, i))
        .collect();
    regions
        .into_iter()
        .enumerate()
        .filter(|(i, r)| last[&r.start_addr] == *i)
        .map(|(_, r)| r)
        .collect()
}
