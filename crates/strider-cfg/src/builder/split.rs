use petgraph::graph::NodeIndex;
use petgraph::visit::EdgeRef;

use super::Builder;
use crate::types::{PcodeInsnAddr, Region, RegionTerminator};
use anyhow::anyhow;

use crate::Result;

impl<R: rsleigh::MemReader> Builder<'_, R> {
    /// Splits at `addr` into a first half (instructions before `addr`, given a
    /// fresh [`NodeIndex`]) and a second half (from `addr` on, RETAINING
    /// `region_id`).
    ///
    /// The second half keeps the id so outgoing edges and work-queue entries
    /// naming `region_id` need no fixup.
    ///
    /// A no-op split returns `region_id` unchanged.
    pub(super) fn split_region(
        &mut self,
        region_id: NodeIndex,
        addr: PcodeInsnAddr,
    ) -> Result<NodeIndex> {
        let second_region = self
            .region_graph
            .node_weight_mut(region_id)
            .ok_or_else(|| anyhow!("invalid region index {region_id:?}"))?;
        // Caller-guaranteed: `Builder::explore` routes a target off every
        // instruction boundary elsewhere, since no split can express it.
        let split_index = second_region
            .insn_index_at(addr)
            .ok_or_else(|| {
                let a = addr.machine_addr.addr;
                anyhow!(
                    "split address {a:#x} (pcode {}) is not an instruction boundary of region {region_id:?}",
                    addr.insn_index,
                )
            })?;
        // `addr == start_addr` (index 0) is the region's own start, resolved as
        // an edge before reaching here; guard defensively.  `insn_index_at`
        // returns an index below the length, so the second half is always
        // non-empty.
        if split_index == 0 {
            return Ok(region_id);
        }
        // `split_off` returns the at-and-after elements, leaving the earlier
        // ones behind, so swap to keep `second_region`'s identity while giving
        // it the second half of the stream.
        let upper = second_region.insns.split_off(split_index);
        // This mutates in place, bypassing `add_region`'s empty-region guard.
        debug_assert!(
            !upper.is_empty(),
            "split_region produced an empty second half (split_index={split_index}): \
             would bypass add_region's empty-region invariant"
        );
        let first_region_insns = std::mem::replace(&mut second_region.insns, upper);
        // The second half never reaches `add_region`, which is where the other
        // regions get this checked.
        debug_assert!(
            second_region.insns_are_ascending(),
            "split_region produced an out-of-order second half"
        );
        let first_region_start_addr = second_region.start_addr;
        second_region.start_addr = addr;

        // Re-index the now-second region under its new start address.  The
        // first half re-takes `first_region_start_addr` via `add_region` below.
        self.start_addr_to_region_id.insert(addr, region_id);
        // A shadowed region keeps its shadower in whichever half now covers it,
        // and the halves are only known to `find_region_containing_addr`
        // through `shadowed_starts`.  The split moved the second half's start,
        // so record the new one: missing it makes the lookup answer `None` for
        // bytes that half really owns, which decodes a fresh region INSIDE an
        // instruction.  The old start stays -- an extra probe costs nothing,
        // a missing one corrupts the graph.
        if self.shadowed_starts.contains(&first_region_start_addr) {
            self.shadowed_starts.insert(addr);
        }

        // The first half always falls through into the second, which keeps
        // `region_id` and therefore the original terminator.
        let first_region = self.add_region(Region {
            start_addr: first_region_start_addr,
            insns: first_region_insns,
            empty_span_len: 0,
            terminator: RegionTerminator::Unconditional,
        })?;
        // Both halves decoded in the same mode. Without carrying it, the first
        // half has no recorded mode and `explore`'s clash check reads `None`,
        // so a later wrong-mode arrival at this address goes unreported for no
        // reason other than that some unrelated target split the region.
        if let Some(&mode) = self.region_isa_mode.get(&region_id) {
            self.region_isa_mode.insert(first_region, mode);
        }

        let parent_edges: Vec<_> = self
            .region_graph
            .edges_directed(region_id, petgraph::Incoming)
            .map(|e| (e.id(), e.source()))
            .collect();

        // Re-target incoming edges onto the first half.  It keeps the original
        // `start_addr`, so a parent `CondBranch`'s address-valued
        // `true_target` still resolves correctly.
        //
        // A `seat_non_boundary_target` edge names an address INSIDE the region
        // instead of its start, so only that address decides which half owns
        // it.  Moved to the first half while the second kept those bytes, the
        // branch is left with no successor containing its target and
        // `region_if` answers `None` for the taken side.
        for (edge_id, parent_id) in parent_edges {
            if self
                .non_boundary_seats
                .get(&edge_id)
                .is_some_and(|&seated| seated >= addr)
            {
                continue;
            }
            let moved = self.region_graph.add_edge(parent_id, first_region, ());
            if let Some(seated) = self.non_boundary_seats.remove(&edge_id) {
                self.non_boundary_seats.insert(moved, seated);
            }
            self.region_graph.remove_edge(edge_id);
        }
        self.region_graph.add_edge(first_region, region_id, ());
        Ok(region_id)
    }
}
