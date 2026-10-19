//! Which region owns an address.

use std::collections::{BTreeMap, BTreeSet};
use std::ops::{Bound, Range};

use petgraph::graph::NodeIndex;

use crate::types::{PcodeInsnAddr, Region, RegionGraph};

#[cfg(test)]
thread_local! {
    /// Regions [`RegionIndex::containing`] tested for containment since a
    /// test last cleared it.
    pub(crate) static REGION_PROBES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Region starts, and what an overlap leaves a lookup to re-probe.
///
/// The greatest start at or below an address need not own it: regions are
/// disjoint only while every target lands on an instruction boundary, and one
/// starting inside another region's last instruction shadows that region over
/// the rest of those bytes. The owner is the containing region with the
/// greatest start.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct RegionIndex {
    /// One region per start address.
    starts: BTreeMap<PcodeInsnAddr, NodeIndex>,
    /// Starts of the regions another region begins inside of: the only ones a
    /// lookup miss has to re-probe.
    shadowed: BTreeSet<PcodeInsnAddr>,
    /// Longest span of a shadowed region, which bounds a miss's reverse walk.
    /// Monotone: a split only shortens both halves.
    ///
    /// Non-zero without overlapping instruction streams too: splitting at a
    /// pcode index INSIDE one machine instruction leaves the first half's last
    /// entry carrying the whole machine-instruction length, so its span reaches
    /// past the second half's start.  That shadow is bounded by one instruction
    /// length.
    max_shadowed_span: u64,
}

impl RegionIndex {
    /// Indexes `id`, already in `g`, under its start, recording both overlap
    /// directions: a region it starts inside of, and one already starting
    /// inside it.
    pub(crate) fn insert(&mut self, g: &RegionGraph, id: NodeIndex) {
        let start = g[id].start_addr;
        let span = g[id].span_len();
        // Probed before `start` is indexed, so `id` cannot find itself.
        if let Some((_, shadowed)) = self.containing(g, start) {
            self.max_shadowed_span = self.max_shadowed_span.max(shadowed.span_len());
            self.shadowed.insert(shadowed.start_addr);
        }
        // Saturating here would make `end` compare equal to (or below) `start`
        // for a region at the top of the address space, and `range` panics on an
        // empty exclusive pair. Overflow means "to the end of memory", so the
        // shadow check still runs rather than being skipped.
        let end = match start.machine_addr.addr.checked_add(span) {
            Some(end) => Bound::Excluded(PcodeInsnAddr::at_machine_start(end)),
            None => Bound::Unbounded,
        };
        if self
            .starts
            .range((Bound::Excluded(start), end))
            .next()
            .is_some()
        {
            self.max_shadowed_span = self.max_shadowed_span.max(span);
            self.shadowed.insert(start);
        }
        self.starts.insert(start, id);
    }

    /// The region starting exactly at `start`.
    pub(crate) fn get(&self, start: PcodeInsnAddr) -> Option<NodeIndex> {
        self.starts.get(&start).copied()
    }

    /// Every indexed start in `range`, ascending.
    pub(crate) fn starts_in(
        &self,
        range: Range<PcodeInsnAddr>,
    ) -> impl Iterator<Item = (PcodeInsnAddr, NodeIndex)> + '_ {
        self.starts.range(range).map(|(&start, &id)| (start, id))
    }

    /// Indexes `id`, the second half of a split, at `new`; the first half
    /// re-takes `old` through [`Self::insert`].
    ///
    /// A shadowed region keeps its shadower in whichever half now covers it,
    /// and the halves are known to [`Self::containing`] only through
    /// `shadowed`. Missing the new start makes the lookup answer `None` for
    /// bytes that half owns, which decodes a fresh region INSIDE an
    /// instruction. The old start stays: an extra probe costs nothing, a
    /// missing one corrupts the graph.
    pub(crate) fn rekey_split(&mut self, old: PcodeInsnAddr, new: PcodeInsnAddr, id: NodeIndex) {
        self.starts.insert(new, id);
        if self.shadowed.contains(&old) {
            self.shadowed.insert(new);
        }
    }

    /// The region owning `addr`: of those containing it, the one with the
    /// greatest start.
    ///
    /// Once the greatest start at or below `addr` does not own it, only a
    /// shadowed region can, and that reaches at most `max_shadowed_span` bytes
    /// below `addr`, which bounds how far back to look. The walk visits only
    /// `shadowed`: bounding by a single global span alone would make one
    /// overlapping address anywhere in the function re-probe every region
    /// within that span, on every miss.
    pub(crate) fn containing<'g>(
        &self,
        g: &'g RegionGraph,
        addr: PcodeInsnAddr,
    ) -> Option<(NodeIndex, &'g Region)> {
        let (&greatest, &greatest_id) = self.starts.range(..=addr).next_back()?;
        if let Some(owner) = owning(g, greatest_id, addr) {
            return Some(owner);
        }
        let floor = PcodeInsnAddr::at_machine_start(
            addr.machine_addr
                .addr
                .saturating_sub(self.max_shadowed_span),
        );
        if floor >= greatest {
            return None;
        }
        self.shadowed
            .range(floor..greatest)
            .rev()
            .filter_map(|start| self.starts.get(start))
            .find_map(|&id| owning(g, id, addr))
    }

    /// Drops every start whose region `g` no longer holds.
    pub(crate) fn retain_live(&mut self, g: &RegionGraph) {
        self.starts.retain(|_, id| g.contains_node(*id));
        let starts = &self.starts;
        self.shadowed.retain(|start| starts.contains_key(start));
    }
}

fn owning(g: &RegionGraph, id: NodeIndex, addr: PcodeInsnAddr) -> Option<(NodeIndex, &Region)> {
    #[cfg(test)]
    REGION_PROBES.with(|n| n.set(n.get() + 1));
    let region = g.node_weight(id)?;
    region.contains_addr(addr).then_some((id, region))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{addr, make_cfg, make_region};

    /// A wide region and one starting inside its instruction, added in that
    /// order: the greatest start owns the bytes both cover, the lower index
    /// does not.
    #[test]
    fn region_containing_answers_the_greatest_start() {
        let mut g = RegionGraph::new();
        let mut wide = make_region(&[(0x1000, 0)]);
        wide.insns[0].len = 10;
        let wide = g.add_node(wide);
        let inner = g.add_node(make_region(&[(0x1005, 0)]));
        let cfg = make_cfg(g, wide);
        assert_eq!(cfg.region_containing(addr(0x1002, 0)), Some(wide));
        assert_eq!(cfg.region_containing(addr(0x1005, 0)), Some(inner));
        assert_eq!(cfg.region_containing(addr(0x1007, 0)), Some(wide));
        assert_eq!(cfg.region_containing(addr(0x100a, 0)), None);
    }

    /// A last instruction crossing the top of the address space owns every
    /// byte above its start, and a region AT `u64::MAX`, whose end does not
    /// fit, still indexes.
    #[test]
    fn region_containing_at_the_top_of_the_address_space() {
        let mut g = RegionGraph::new();
        let mut wide = make_region(&[(u64::MAX - 3, 0)]);
        wide.insns[0].len = 10;
        let wide = g.add_node(wide);
        let inner = g.add_node(make_region(&[(u64::MAX - 1, 0)]));
        let top = g.add_node(make_region(&[(u64::MAX, 0)]));
        let cfg = make_cfg(g, wide);
        assert_eq!(cfg.region_containing(addr(u64::MAX - 2, 0)), Some(wide));
        assert_eq!(cfg.region_containing(addr(u64::MAX - 1, 0)), Some(inner));
        assert_eq!(cfg.region_containing(addr(u64::MAX, 0)), Some(top));
    }

    /// A region the graph no longer holds leaves no start behind.
    #[test]
    fn retain_live_drops_the_starts_of_removed_regions() {
        let mut g = RegionGraph::new();
        let kept = g.add_node(make_region(&[(0x1000, 0)]));
        let dropped = g.add_node(make_region(&[(0x2000, 0)]));
        let mut index = RegionIndex::default();
        index.insert(&g, kept);
        index.insert(&g, dropped);
        assert!(g.remove_node(dropped).is_some());
        index.retain_live(&g);
        assert_eq!(index.get(addr(0x2000, 0)), None);
        assert_eq!(index.get(addr(0x1000, 0)), Some(kept));
    }

    /// A hit at the greatest start and a miss past every region each probe one
    /// region however many precede the address: the chain `strider-py`'s
    /// `region_at` was timed over, one four-byte region per branch.
    #[test]
    fn region_containing_probes_one_region_however_many_precede() {
        for regions in [10u64, 1_500] {
            let mut g = RegionGraph::new();
            let mut ids = Vec::new();
            for i in 0..regions {
                let base = 0x1000 + 4 * i;
                let mut region = make_region(&[(base, 0), (base + 2, 0)]);
                region.insns[0].len = 2;
                region.insns[1].len = 2;
                ids.push(g.add_node(region));
            }
            let cfg = make_cfg(g, ids[0]);
            let end = 0x1000 + 4 * regions;
            for (query, owner) in [(end - 4, ids.last().copied()), (end + 0x10000, None)] {
                REGION_PROBES.with(|n| n.set(0));
                assert_eq!(
                    cfg.region_containing(addr(query, 0)),
                    owner,
                    "{regions} regions"
                );
                assert_eq!(
                    REGION_PROBES.with(std::cell::Cell::get),
                    1,
                    "{regions} regions, {query:#x}"
                );
            }
        }
    }
}
