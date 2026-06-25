use petgraph::{graph::NodeIndex, visit::EdgeRef};

use super::Builder;
use crate::types::{PcodeInsnAddr, Region, RegionTerminator};
use anyhow::anyhow;

use crate::Result;

impl<R: rsleigh::MemReader> Builder<'_, R> {
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
    /// 2. An (unweighted) edge is added from first → second; the first half's
    ///    `Unconditional` terminator classifies the transfer.
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
        //
        // The below-every-insn case (no `rposition` match) IS reachable from
        // the normal call path: a hole-rounded region's `start_addr` can sit
        // below its first surviving insn (e.g. 0x1008 while insns=[0x100c]),
        // and `Region::contains_addr` reports true across the phantom span
        // [start_addr, first_insn).  A branch target landing there routes
        // `explore` here.  Such an address already belongs to this region's
        // start, so the correct response is a no-op split (index 0), NOT a hard
        // error that would abort the whole function's CFG build.  Only a target
        // genuinely below `start_addr` is an error.
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
        // No-op when `addr` lands at/past the last insn — the second
        // region would be empty + retain the original (non-Unconditional)
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
        // Defend `add_region`'s empty-region invariant at this in-place
        // mutation site: the two early returns above (`split_index == 0`
        // and `split_index >= len`) guarantee `0 < split_index < len`, so
        // the second half (elements at-and-after `split_index`) is always
        // non-empty and keeps its original (possibly non-`Unconditional`)
        // terminator legally.  A future change to the index arithmetic that
        // left this half empty would silently bypass `add_region`'s guard,
        // so assert it here rather than trust the call path.
        debug_assert!(
            !upper.is_empty(),
            "split_region produced an empty second half (split_index={split_index}) — \
             would bypass add_region's empty-region invariant"
        );
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
            terminator: RegionTerminator::Unconditional,
        })?;

        // second region inherits all parents of the original region
        let parent_edges: Vec<_> = self
            .region_graph
            .edges_directed(region_id, petgraph::Incoming)
            .map(|e| (e.id(), e.source()))
            .collect();

        // Re-target each incoming edge from the original (now second) region onto
        // the freshly-created first region, then drop the original edge.  Edges
        // are unweighted; the first half keeps the original `start_addr`, so a
        // parent `CondBranch`'s `true_target` (an address) still resolves here.
        for (edge_id, parent_id) in parent_edges {
            self.region_graph.add_edge(parent_id, first_region, ());
            self.region_graph.remove_edge(edge_id);
        }
        // link the first and the second regions with fallthrough
        self.region_graph.add_edge(first_region, region_id, ());
        Ok(region_id)
    }
}
