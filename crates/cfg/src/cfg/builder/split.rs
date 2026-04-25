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
    /// - any work-queue entries that still reference `region_id` as a parent
    ///   (including the popped item that triggered this split — its parent
    ///   pointer carries forward to the second half automatically).
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
        // `split_off(at)` returns elements at-and-after `at`, leaving elements before `at`
        // in `second_region.insns`. Swap them so `second_region` keeps its identity but
        // owns the second half of the original instruction stream.
        let upper = second_region.insns.split_off(split_index);
        let first_region_insns = std::mem::replace(&mut second_region.insns, upper);
        let first_region_start_addr = second_region.start_addr;
        second_region.start_addr = addr;

        // Re-index the (now-second) region under its new start address.
        self.start_addr_to_region_id.insert(addr, region_id);

        let first_region = self.add_region(Region {
            start_addr: first_region_start_addr,
            insns: first_region_insns,
            ends_with_tail_call: false,
        })?;

        // second region inherits all parents of the original region
        let parent_edges: Vec<_> = self
            .graph
            .edges_directed(region_id, petgraph::Incoming)
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
            .add_edge(first_region, region_id, RegionEdgeKind::Fallthrough);
        Ok(region_id)
    }
}
