use std::collections::BTreeSet;

use crate::{MemReadError, MemRegion, RegionIndex, Result, collapse_equal_starts};

/// Lookup table over a set of possibly-overlapping [`MemRegion`]s.
///
/// Regions sharing a start address collapse, last-inserted wins. A read is
/// resolved through a [`RegionIndex`], so it costs O(log n) whether the set is
/// disjoint or nests.
#[derive(Debug)]
pub struct MemRegionsLookupTable {
    /// Ascending by start address, one region per start.
    regions: Vec<MemRegion>,
    pub(crate) index: RegionIndex,
    /// One `regions` index per distinct mapping behind them, so
    /// [`check_unchanged`] costs a stat per mapping and no scan at all.
    ///
    /// [`check_unchanged`]: Self::check_unchanged
    mappings: Vec<usize>,
    /// Bytes in the largest region: the most one read can return.
    max_region_len: usize,
}

impl MemRegionsLookupTable {
    /// Two regions sharing a start address collapse to the later one.
    pub fn new<I: IntoIterator<Item = MemRegion>>(regions: I) -> Self {
        let mut regions = collapse_equal_starts(regions.into_iter().collect());
        regions.sort_unstable_by_key(MemRegion::start_addr);
        let index = RegionIndex::new(&regions);
        let mut seen: BTreeSet<usize> = BTreeSet::new();
        let mappings = regions
            .iter()
            .enumerate()
            .filter(|(_, r)| r.mapping_id().is_some_and(|id| seen.insert(id)))
            .map(|(i, _)| i)
            .collect();
        let max_region_len = regions
            .iter()
            .map(|r| (r.end_addr() - r.start_addr()) as usize)
            .max()
            .unwrap_or(0);
        Self {
            regions,
            index,
            mappings,
            max_region_len,
        }
    }

    /// [`MemRegion::check_unchanged`] over the table, one `stat` per distinct
    /// mapping rather than per region: an image's regions all share one.
    ///
    /// The mappings are the ones the regions carried at construction, which
    /// are every mapping the table can serve a byte from, so a changed file
    /// is caught wherever it backs the table. Which changes a `stat` can see
    /// at all is [`MemRegion::check_unchanged`]'s.
    ///
    /// # Errors
    ///
    /// When any mapped file behind the table changed since it was mapped.
    pub fn check_unchanged(&self) -> Result<()> {
        Self::check_all_unchanged([self])
    }

    /// [`check_unchanged`] over several tables, one `stat` per mapping however
    /// many of them share it.
    ///
    /// # Errors
    ///
    /// When any mapped file behind the tables changed since it was mapped.
    ///
    /// [`check_unchanged`]: Self::check_unchanged
    pub fn check_all_unchanged<'a>(tables: impl IntoIterator<Item = &'a Self>) -> Result<()> {
        let mut seen = BTreeSet::new();
        for table in tables {
            for &i in &table.mappings {
                let region = &table.regions[i];
                if region.mapping_id().is_some_and(|id| seen.insert(id)) {
                    region.check_unchanged()?;
                }
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

    /// [`Self::read`] of up to `size` bytes into a buffer no larger than one
    /// region, so a huge `size` allocates nothing a read cannot fill.
    pub fn read_vec(&self, addr: u64, size: usize) -> Option<Vec<u8>> {
        let mut buf = vec![0u8; size.min(self.max_region_len)];
        let n = self.read(addr, &mut buf)?;
        buf.truncate(n);
        Some(buf)
    }

    /// Fill-all-or-error read: copies the mapped bytes into `buf` **raw**, with
    /// no endianness swap. Callers wanting an integer decode them themselves.
    /// A short fill errors rather than truncating.
    ///
    /// # Errors
    ///
    /// When `addr` is unmapped, or the request straddles a region's end or a
    /// relocation field the loader could not compute, so fewer than
    /// `buf.len()` bytes are available.
    pub fn read_exact(&self, addr: u64, buf: &mut [u8]) -> anyhow::Result<()> {
        let want = buf.len();
        let got = self.read(addr, buf).ok_or_else(|| self.unmapped(addr))?;
        if got != want {
            // Checked like every neighbouring site: a region reaching the top
            // of the address space makes this the one unchecked add, and under
            // the release profile's overflow checks that is a panic.
            let stop = addr.checked_add(got as u64).ok_or_else(|| {
                anyhow::anyhow!("read at {addr:#x} of {got} bytes overflows the address space")
            })?;
            if let Some(r_type) = self.unmodelled_relocation_at(stop) {
                anyhow::bail!(
                    "read at {addr:#x} runs into {stop:#x}, a relocation of type {r_type} \
                     this loader does not compute"
                );
            }
            anyhow::bail!("read at {addr:#x} spans past mapped memory: got {got} of {want} bytes");
        }
        Ok(())
    }

    /// The `r_type` of the uncomputed relocation field covering `addr` in the
    /// region a one-byte read there would be served by.
    pub fn unmodelled_relocation_at(&self, addr: u64) -> Option<u32> {
        let i = self.index.covering(addr, 1).next()?;
        self.regions[i].unmodelled_relocation_at(addr)
    }

    /// Why a read at `addr` served nothing.
    fn unmapped(&self, addr: u64) -> anyhow::Error {
        match self.unmodelled_relocation_at(addr) {
            Some(r_type) => anyhow::anyhow!(
                "address {addr:#x} holds a relocation of type {r_type} this loader does not compute"
            ),
            None => anyhow::anyhow!("address {addr:#x} is not mapped"),
        }
    }
}

/// Instruction fetch: [`MemRegionsLookupTable::read`], erring with what a
/// failed read served nothing for.
impl rsleigh::MemReader for MemRegionsLookupTable {
    type Err = MemReadError;

    fn read(
        &self,
        addr: rsleigh::VnAddr,
        out_buf: &mut [u8],
    ) -> std::result::Result<usize, Self::Err> {
        MemRegionsLookupTable::read(self, addr.off, out_buf)
            .ok_or_else(|| MemReadError(self.unmapped(addr.off)))
    }
}
