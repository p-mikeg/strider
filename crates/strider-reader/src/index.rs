use crate::MemRegion;

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
        let ranges: Vec<(u64, u64)> = regions
            .iter()
            .map(|r| (r.start_addr(), r.end_addr()))
            .collect();
        Self::from_ranges(&ranges)
    }

    /// The index over `[start, end)` ranges, yielding indices into `ranges`
    /// exactly as [`new`](Self::new) yields them into a region slice.
    pub fn from_ranges(ranges: &[(u64, u64)]) -> Self {
        let mut order: Vec<usize> = (0..ranges.len()).collect();
        // Stable, so equal starts keep slice order.
        order.sort_by_key(|&i| ranges[i].0);
        let mut reach = 0u64;
        let entries: Vec<IndexEntry> = order
            .iter()
            .map(|&index| {
                reach = reach.max(ranges[index].1);
                IndexEntry {
                    start: ranges[index].0,
                    reach,
                    index,
                }
            })
            .collect();
        let ends: Vec<u64> = order.iter().map(|&i| ranges[i].1).collect();
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
        // zero-length request, which it does not contain. An end past
        // `u64::MAX` matches nothing.
        let (upper, want) = match addr.checked_add(len.max(1)) {
            Some(want) => (self.entries.partition_point(|e| e.start <= addr), want),
            None => (0, 0),
        };
        Covering {
            index: self,
            upper,
            want,
        }
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

#[cfg(test)]
thread_local! {
    /// `MaxEnd::descend` calls since a test last cleared it.
    pub(crate) static DESCENTS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
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
        #[cfg(test)]
        DESCENTS.with(|n| n.set(n.get() + 1));
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
