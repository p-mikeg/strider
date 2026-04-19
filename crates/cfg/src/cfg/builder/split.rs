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
        // 4. The parent of the popped value from that called the split should also point to the second region

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

        // Move the parent edges to be in the first region instead of the first one
        for (edge_id, parent_id, edge_data) in parent_edges {
            // re-add edge from second_region to the child
            self.graph.add_edge(parent_id, first_region, edge_data);

            // remove the original edge
            self.graph.remove_edge(edge_id);
        }
        // link the first and the second regions with fallthrough
        self.graph
            .add_edge(first_region, second_region_id, RegionEdgeKind::Fallthrough);
        Ok(second_region_id)
    }
}

#[cfg(test)]
mod tests {
    use super::super::testing::*;
    use super::*;
    use petgraph::visit::{EdgeRef, IntoEdgeReferences};

    /// Splitting at the region's own start address is a no-op and returns the
    /// original `NodeIndex`.
    #[test]
    fn split_region_at_start_is_noop() {
        let mut b = make_builder(0x1000);
        let id = b
            .add_region(make_region(&[(0x1000, 0), (0x1004, 0), (0x1008, 0)]))
            .unwrap();
        let result = b.split_region(id, addr(0x1000, 0)).unwrap();

        assert_eq!(result, id, "split at start must return original id");
        assert_eq!(b.graph.node_count(), 1, "no new region should be created");
    }

    /// Splitting at an interior address produces two regions.  The second half
    /// keeps the original `NodeIndex`; a new id is created for the first half.
    #[test]
    fn split_region_creates_two_regions() {
        let mut b = make_builder(0x1000);
        let original = b
            .add_region(make_region(&[
                (0x1000, 0),
                (0x1004, 0),
                (0x1008, 0),
                (0x100c, 0),
            ]))
            .unwrap();
        let second = b.split_region(original, addr(0x1008, 0)).unwrap();

        // The second half keeps the original NodeIndex
        assert_eq!(second, original);
        assert_eq!(b.graph.node_count(), 2);
    }

    /// After a split the second region starts at the split address and the
    /// first region ends just before it.
    #[test]
    fn split_region_correct_addr_ranges() {
        let mut b = make_builder(0x1000);
        let original = b
            .add_region(make_region(&[
                (0x1000, 0),
                (0x1004, 0),
                (0x1008, 0),
                (0x100c, 0),
            ]))
            .unwrap();
        b.split_region(original, addr(0x1008, 0)).unwrap();

        // second half (original id) starts at split point
        assert_eq!(b.graph[original].start_addr, addr(0x1008, 0));
        assert_eq!(b.graph[original].insns.len(), 2);

        // first half starts at the original start
        let first_id = b.start_addr_to_region_id[&addr(0x1000, 0)];
        assert_eq!(b.graph[first_id].start_addr, addr(0x1000, 0));
        assert_eq!(b.graph[first_id].insns.len(), 2);
    }

    /// A `Fallthrough` edge must connect the first half to the second half
    /// after the split.
    #[test]
    fn split_region_adds_fallthrough_edge() {
        let mut b = make_builder(0x1000);
        let original = b
            .add_region(make_region(&[(0x1000, 0), (0x1004, 0), (0x1008, 0)]))
            .unwrap();
        b.split_region(original, addr(0x1008, 0)).unwrap();

        let edges: Vec<_> = b.graph.edge_references().collect();
        assert_eq!(edges.len(), 1, "exactly one edge after split");
        assert_eq!(*edges[0].weight(), RegionEdgeKind::Fallthrough);
        assert_eq!(
            edges[0].target(),
            original,
            "edge must point to the second half"
        );
    }

    /// Incoming edges to the original region are rewired to the first half.
    #[test]
    fn split_region_rewires_incoming_edges() {
        let mut b = make_builder(0x1000);
        // Region A (parent)
        let a = b.add_region(make_region(&[(0x0ff0, 0)])).unwrap();
        // Region B (to be split); A → B via Branch
        let b_id = b
            .add_region(make_region(&[(0x1000, 0), (0x1004, 0), (0x1008, 0)]))
            .unwrap();
        b.graph.add_edge(a, b_id, RegionEdgeKind::Branch);

        // Split B at 0x1004
        b.split_region(b_id, addr(0x1004, 0)).unwrap();

        // The original incoming Branch edge must now point to the first half
        let first = b.start_addr_to_region_id[&addr(0x1000, 0)];
        let incoming: Vec<_> = b.graph.edges_directed(first, petgraph::Incoming).collect();
        assert_eq!(incoming.len(), 1);
        assert_eq!(*incoming[0].weight(), RegionEdgeKind::Branch);
        assert_eq!(incoming[0].source(), a);

        // The second half (b_id) must NOT have the old Branch incoming edge
        let second_incoming: Vec<_> = b
            .graph
            .edges_directed(b_id, petgraph::Incoming)
            .filter(|e| *e.weight() == RegionEdgeKind::Branch)
            .collect();
        assert!(second_incoming.is_empty());
    }
}
