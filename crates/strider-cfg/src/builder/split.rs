use petgraph::graph::NodeIndex;
use petgraph::visit::EdgeRef;

use super::Builder;
use crate::types::{PcodeInsnAddr, Region, RegionTerminator};
use anyhow::anyhow;

use crate::Result;

impl<R: rsleigh::MemReader> Builder<'_, R> {
    /// Splits at `addr` into a first half (before `addr`, a fresh
    /// [`NodeIndex`]) and a second half that keeps `region_id`, so its
    /// outgoing edges and queued work need no fixup. Returns `region_id`.
    pub(super) fn split_region(
        &mut self,
        region_id: NodeIndex,
        addr: PcodeInsnAddr,
    ) -> Result<NodeIndex> {
        let second_region = &mut self.region_graph[region_id];
        let split_index = second_region
            .insn_index_at(addr)
            .ok_or_else(|| {
                let a = addr.machine();
                anyhow!(
                    "split address {a:#x} (pcode {}) is not an instruction boundary of region {region_id:?}",
                    addr.insn_index,
                )
            })?;
        // The region's own start: nothing to split.
        if split_index == 0 {
            return Ok(region_id);
        }
        // The smaller half moves to a fresh allocation, but `drain` still
        // shifts the tail: repeated lowest-first splits of one region (arm
        // order, or direct targets popped ascending) are O(n^2).
        let insns = &mut second_region.insns;
        let first_region_insns: Vec<_> = if split_index <= insns.len() / 2 {
            insns.drain(..split_index).collect()
        } else {
            let upper = insns.split_off(split_index);
            std::mem::replace(insns, upper)
        };
        debug_assert!(
            !second_region.insns.is_empty(),
            "split_region produced an empty second half (split_index={split_index}): \
             would bypass add_region's empty-region invariant"
        );
        debug_assert!(
            second_region.insns_are_ascending(),
            "split_region produced an out-of-order second half"
        );
        let first_region_start_addr = second_region.start_addr;
        let isa_mode = second_region.isa_mode;
        second_region.start_addr = addr;

        self.index
            .rekey_split(first_region_start_addr, addr, region_id);

        let first_region = self.add_region(Region {
            start_addr: first_region_start_addr,
            insns: first_region_insns,
            empty_span_len: 0,
            terminator: RegionTerminator::Unconditional,
            isa_mode,
        })?;

        let parent_edges: Vec<_> = self
            .region_graph
            .edges_directed(region_id, petgraph::Incoming)
            .map(|e| (e.id(), e.source()))
            .collect();

        // Incoming edges move to the first half. A `StableDiGraph` edge id
        // survives another edge's removal.
        for (edge_id, parent_id) in parent_edges {
            self.region_graph.add_edge(parent_id, first_region, ());
            self.region_graph.remove_edge(edge_id);
        }
        self.region_graph.add_edge(first_region, region_id, ());
        Ok(region_id)
    }
}
