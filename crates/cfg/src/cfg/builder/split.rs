use petgraph::graph::NodeIndex;
use petgraph::visit::EdgeRef;

use super::Builder;
use crate::cfg::types::{PcodeInsnAddr, Region, RegionEdgeKind};
use crate::error::{ErrorKind, Result};

impl<R: rsleigh::MemReader> Builder<R> {
    /// Splits the region identified by `region_id` at `addr`, creating two
    /// regions:
    ///
    /// - **first**: instructions *before* `addr` — gets a new [`NodeIndex`].
    /// - **second**: instructions *from* `addr` onwards — **retains** `region_id`.
    ///
    /// Retaining `region_id` for the second half avoids having to update:
    /// - outgoing edges (children) of the original region, and
    /// - any work-queue entries that still reference `region_id` as a parent.
    ///
    /// The following fixups ARE performed manually:
    /// 1. Incoming edges (parents) are rewired to the first region.
    /// 2. A [`RegionEdgeKind::Fallthrough`] edge is added from first → second.
    /// 3. The `start_addr_to_region_id` map is updated for both halves.
    ///
    /// Returns `region_id` (the second region) on success, or `region_id`
    /// unchanged when `addr` is already the region start (no-op split).
    pub(super) fn split_region(
        &mut self,
        region_id: NodeIndex,
        addr: PcodeInsnAddr,
    ) -> Result<NodeIndex> {
        // The idea here is to swap the region_id to be the **SECOND** region after the split and create a new one for the first one
        // Why? there are 4 things that break when we want to change the region_id
        // 1. The parents of the current region_id should be those of the first region - we will fix it by hand
        // 2. The children of the current region_id should be those of the second region - solved due to replacement
        // 3. The items in the queue that use region_id as parent should point to the second region - solved due to replacement
        // 4. The parent of the popped work-queue item that triggered the split should also point to the second region

        let second_region = self
            .graph
            .node_weight_mut(region_id)
            .ok_or(ErrorKind::InvalidRegion(region_id))?;
        let split_index = second_region
            .insns
            .iter()
            .position(|insn| insn.addr == addr)
            .ok_or(ErrorKind::FailedSplitingRegion(region_id, addr))?;

        if split_index == 0 {
            return Ok(region_id);
        }
        // split the insns in 2 based on the split index -  split_off stores the first part in place
        // so we should replace the 2 values
        let second_region_insns = second_region.insns.split_off(split_index);
        let first_region_insns = std::mem::replace(&mut second_region.insns, second_region_insns);
        let second_region_id = region_id;
        let first_region_start_addr = second_region.start_addr;
        second_region.start_addr = addr;

        // We need to update the region location in the mapping to get the correct one when accessed later
        self.start_addr_to_region_id
            .insert(second_region.start_addr, second_region_id);

        let first_region = self.add_region(Region {
            start_addr: first_region_start_addr,
            insns: first_region_insns,
            ends_with_tail_call: false,
        })?;

        // second region inherits all parents of the original region
        let parent_edges: Vec<_> = self
            .graph
            .edges_directed(second_region_id, petgraph::Incoming)
            .map(|e| (e.id(), e.source(), *e.weight()))
            .collect();

        // Re-target each incoming edge from the original (now second) region onto
        // the freshly-created first region, then drop the original edge.
        for (edge_id, parent_id, edge_data) in parent_edges {
            self.graph.add_edge(parent_id, first_region, edge_data);
            self.graph.remove_edge(edge_id);
        }
        // link the first and the second regions with fallthrough
        self.graph
            .add_edge(first_region, second_region_id, RegionEdgeKind::Fallthrough);
        Ok(second_region_id)
    }
}
