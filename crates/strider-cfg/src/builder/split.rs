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
        let second_region = &mut self.region_graph[region_id];
        // Caller-guaranteed: `Builder::explore` routes a target off every
        // instruction boundary elsewhere, since no split can express it.
        let split_index = second_region
            .insn_index_at(addr)
            .ok_or_else(|| {
                let a = addr.machine();
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
        // The smaller half moves to a fresh allocation. `drain` still shifts
        // the tail, so repeated lowest-first splits of one region are O(n^2):
        // `process_branch_indirect` enqueues arms highest first, so the LIFO
        // explores them lowest first.
        let insns = &mut second_region.insns;
        let first_region_insns: Vec<_> = if split_index <= insns.len() / 2 {
            insns.drain(..split_index).collect()
        } else {
            let upper = insns.split_off(split_index);
            std::mem::replace(insns, upper)
        };
        // This mutates in place, bypassing `add_region`'s empty-region guard.
        debug_assert!(
            !second_region.insns.is_empty(),
            "split_region produced an empty second half (split_index={split_index}): \
             would bypass add_region's empty-region invariant"
        );
        // The second half never reaches `add_region`, which is where the other
        // regions get this checked.
        debug_assert!(
            second_region.insns_are_ascending(),
            "split_region produced an out-of-order second half"
        );
        let first_region_start_addr = second_region.start_addr;
        let isa_mode = second_region.isa_mode;
        second_region.start_addr = addr;

        // Re-index the now-second region under its new start address.  The
        // first half re-takes `first_region_start_addr` via `add_region` below.
        self.index
            .rekey_split(first_region_start_addr, addr, region_id);

        // The first half always falls through into the second, which keeps
        // `region_id` and therefore the original terminator.
        let first_region = self.add_region(Region {
            start_addr: first_region_start_addr,
            insns: first_region_insns,
            empty_span_len: 0,
            terminator: RegionTerminator::Unconditional,
            // Both halves decoded in the same mode.
            isa_mode,
        })?;

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
            // `RegionGraph` is a `StableDiGraph`: `remove_edge` frees the slot
            // into a free list rather than swap-removing, so `moved` stays valid
            // across the removal and the seat keeps pointing at the right edge.
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
