use petgraph::graph::NodeIndex;
use petgraph::visit::EdgeRef;

use super::Builder;
use crate::cfg::types::{PcodeInsnAddr, Region, RegionEdgeKind, RegionTerminator};
use anyhow::anyhow;

use crate::cfg::Result;

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
            .region_graph
            .node_weight_mut(region_id)
            .ok_or_else(|| anyhow!("invalid region index {region_id:?}"))?;
        // Round-down fallback for split addresses that fall in a
        // zero-pcode-op hole.  When `addr` doesn't match any recorded
        // insn (because intervening machine instructions lifted to zero
        // pcode ops — AArch64 `paciasp` / `autiasp`, ARM `bti`, etc.),
        // split after the largest insn whose address is ≤ `addr`.  The
        // second region keeps the requested `addr` as its `start_addr`
        // so future lookups for that exact address resolve to it.
        // Below-every-insn (no `rposition` match) is unreachable from
        // the cfg builder's normal call path — the caller's
        // `contains_addr` check rules it out — but the API is exposed
        // via `test_api`, so a clean error beats a panic.
        let split_index = match second_region
            .insns
            .iter()
            .position(|insn| insn.addr == addr)
        {
            Some(idx) => idx,
            None => second_region
                .insns
                .iter()
                .rposition(|insn| insn.addr <= addr)
                .map(|i| i + 1)
                .ok_or_else(|| {
                    anyhow!(
                        "split address {addr:?} not found in region {region_id:?}'s instruction list"
                    )
                })?,
        };

        if split_index == 0 {
            return Ok(region_id);
        }
        // No-op when `addr` lands at/past the last insn — the second
        // region would be empty + retain the original (non-Branch)
        // terminator, which `add_region` correctly rejects but
        // `split_region` mutates in place rather than going through
        // it.  Returning the original id is the right "split is a
        // no-op" answer; the caller's `contains_addr` check should
        // already preclude a query past the last insn, so this guard
        // is purely defensive.
        if split_index >= second_region.insns.len() {
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

        // The first half always falls through into the second half — the
        // original region's terminator stays put on the second half (which
        // retains `region_id` and therefore the in-place `Region` value the
        // builder originally wrote there).
        let first_region = self.add_region(Region {
            start_addr: first_region_start_addr,
            insns: first_region_insns,
            terminator: RegionTerminator::Fallthrough,
        })?;

        // second region inherits all parents of the original region
        let parent_edges: Vec<_> = self
            .region_graph
            .edges_directed(region_id, petgraph::Incoming)
            .map(|e| (e.id(), e.source(), *e.weight()))
            .collect();

        // Re-target each incoming edge from the original (now second) region onto
        // the freshly-created first region, then drop the original edge.
        for (edge_id, parent_id, edge_data) in parent_edges {
            self.region_graph.add_edge(parent_id, first_region, edge_data);
            self.region_graph.remove_edge(edge_id);
        }
        // link the first and the second regions with fallthrough
        self.region_graph
            .add_edge(first_region, region_id, RegionEdgeKind::Fallthrough);
        Ok(region_id)
    }
}
