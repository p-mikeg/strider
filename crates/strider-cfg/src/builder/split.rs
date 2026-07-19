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
    /// The second half keeps the id so outgoing edges and any work-queue
    /// entries naming `region_id` as a parent need no fixup, including the
    /// popped item that triggered this split.  Incoming edges, the
    /// first-to-second fall-through edge, and both `start_addr_to_region_id`
    /// entries are fixed up by hand below.
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
        // `addr` need not match a recorded insn: intervening machine
        // instructions may have lifted to zero pcode ops (AArch64 `paciasp` /
        // `autiasp`, ARM `bti`), leaving a hole.  Round down and split after
        // the largest insn at or below `addr`, keeping the requested `addr` as
        // the second region's `start_addr` so later lookups resolve to it.
        //
        // The below-every-insn case IS reachable normally: a hole-rounded
        // region's `start_addr` can sit below its first surviving insn (0x1008
        // with insns=[0x100c]), and `contains_addr` reports true across the
        // phantom span [start_addr, first_insn), so a branch target landing
        // there routes here.  That address already belongs to this region's
        // start, so the answer is a no-op split, NOT an error aborting the
        // whole function's CFG build.  Only a target genuinely below
        // `start_addr` is an error.
        let split_index = match second_region
            .insns
            .iter()
            .position(|insn| insn.addr == addr)
        {
            Some(idx) => idx,
            None => match second_region
                .insns
                .iter()
                .rposition(|insn| insn.addr <= addr)
            {
                Some(i) => i + 1,
                None if addr >= second_region.start_addr => 0,
                None => {
                    return Err(anyhow!(
                        "split address {addr:?} not found in region {region_id:?}'s instruction list"
                    ));
                }
            },
        };

        if split_index == 0 {
            return Ok(region_id);
        }
        // Defensive: `contains_addr` should already preclude a query past the
        // last insn.  Splitting there would leave the second region empty
        // while retaining the original non-`Unconditional` terminator, a shape
        // `add_region` rejects but this in-place path would never show it.
        if split_index >= second_region.insns.len() {
            return Ok(region_id);
        }
        // `split_off` returns the at-and-after elements, leaving the earlier
        // ones behind, so swap to keep `second_region`'s identity while giving
        // it the second half of the stream.
        let upper = second_region.insns.split_off(split_index);
        // This mutates in place, bypassing `add_region`'s empty-region guard.
        // The two early returns above pin `0 < split_index < len`, so the
        // second half is non-empty and legally keeps its original terminator.
        // Assert it rather than trust future index arithmetic.
        debug_assert!(
            !upper.is_empty(),
            "split_region produced an empty second half (split_index={split_index}) — \
             would bypass add_region's empty-region invariant"
        );
        let first_region_insns = std::mem::replace(&mut second_region.insns, upper);
        let first_region_start_addr = second_region.start_addr;
        second_region.start_addr = addr;

        // Re-index the now-second region under its new start address.
        self.start_addr_to_region_id.insert(addr, region_id);

        // The first half always falls through into the second, which keeps
        // `region_id` and therefore the original terminator.
        let first_region = self.add_region(Region {
            start_addr: first_region_start_addr,
            insns: first_region_insns,
            terminator: RegionTerminator::Unconditional,
        })?;

        let parent_edges: Vec<_> = self
            .region_graph
            .edges_directed(region_id, petgraph::Incoming)
            .map(|e| (e.id(), e.source()))
            .collect();

        // Re-target incoming edges onto the first half.  It keeps the original
        // `start_addr`, so a parent `CondBranch`'s address-valued
        // `true_target` still resolves correctly.
        for (edge_id, parent_id) in parent_edges {
            self.region_graph.add_edge(parent_id, first_region, ());
            self.region_graph.remove_edge(edge_id);
        }
        self.region_graph.add_edge(first_region, region_id, ());
        Ok(region_id)
    }
}
