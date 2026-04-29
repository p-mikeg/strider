use petgraph::graph::NodeIndex;
use petgraph::visit::EdgeRef;

use super::types::{Region, RegionEdgeKind};
use super::{Cfg, RegionId};
use anyhow::anyhow;

use crate::error::Result;

/// The two successors of a conditional-branch region.
///
/// Returned by [`Cfg::region_if`].
pub struct IfRegionState {
    /// Region reached when the branch condition is *true*, if present.
    pub if_true_region: Option<NodeIndex>,
    /// Region reached when the branch condition is *false* (fall-through), if present.
    pub if_false_region: Option<NodeIndex>,
}

impl<R: rsleigh::MemReader> Cfg<R> {
    /// Returns the sole successor of `region_id` whose edge weight is `kind`,
    /// or `None` if no such edge exists.
    ///
    /// # Errors
    /// Returns [`ErrorKind::DuplicateEdgeKind`] when more than one outgoing
    /// edge of `kind` is attached to `region_id`.
    fn unique_outgoing(&self, region_id: RegionId, kind: RegionEdgeKind) -> Result<Option<NodeIndex>> {
        let mut found: Option<NodeIndex> = None;
        for edge in self.graph.edges_directed(region_id, petgraph::Outgoing) {
            if *edge.weight() != kind {
                continue;
            }
            if found.is_some() {
                return Err(anyhow!("region {region_id:?} has more than one outgoing edge of kind {kind:?}"));
            }
            found = Some(edge.target());
        }
        Ok(found)
    }

    /// Returns the unconditional-branch successor of `region_id`, if any.
    ///
    /// # Errors
    /// Returns [`ErrorKind::DuplicateEdgeKind`] when more than one `Branch`
    /// edge leaves `region_id`.
    pub fn region_branch(&self, region_id: RegionId) -> Result<Option<NodeIndex>> {
        self.unique_outgoing(region_id, RegionEdgeKind::Branch)
    }

    /// Returns the fallthrough successor of `region_id`, if any.
    ///
    /// Used by the analyzer to detect BUG-25-normalised unconditional
    /// branches: a CFG `Branch` p-code op whose target was reclassified
    /// as `Fallthrough` because it pointed at `pc + insn_len`.
    ///
    /// # Errors
    /// Returns [`ErrorKind::DuplicateEdgeKind`] when more than one
    /// `Fallthrough` edge leaves `region_id`.
    pub fn region_fallthrough(&self, region_id: RegionId) -> Result<Option<NodeIndex>> {
        self.unique_outgoing(region_id, RegionEdgeKind::Fallthrough)
    }

    /// Returns both conditional-branch successors of `region_id`.
    ///
    /// # Errors
    /// Returns [`ErrorKind::DuplicateEdgeKind`] when more than one
    /// `IfCaseTrue` or `IfCaseFalse` edge leaves `region_id`.
    pub fn region_if(&self, region_id: RegionId) -> Result<IfRegionState> {
        Ok(IfRegionState {
            if_true_region: self.unique_outgoing(region_id, RegionEdgeKind::IfCaseTrue)?,
            if_false_region: self.unique_outgoing(region_id, RegionEdgeKind::IfCaseFalse)?,
        })
    }

    /// Iterates over all [`Region`]s in the CFG (unordered).
    pub fn regions(&self) -> impl Iterator<Item = &Region> {
        self.graph.node_weights()
    }

    /// Iterates over the [`RegionId`] of every region in the CFG (unordered).
    pub fn region_ids(&self) -> impl Iterator<Item = RegionId> {
        self.graph.node_indices()
    }

    /// Returns the number of incoming edges (predecessors) for
    /// `region_id`.  Used by the strider fixed-point orchestrator's
    /// `RegionIrCache` to detect when a cached region has gained a
    /// new predecessor across iterations.
    #[must_use]
    pub fn predecessor_count(&self, region_id: RegionId) -> usize {
        self.graph
            .neighbors_directed(region_id, petgraph::Incoming)
            .count()
    }

    /// Returns the `RegionId` of the region whose **start machine
    /// address** equals `addr`, or `None` if no such region exists.
    ///
    /// Used by the strider fixed-point orchestrator's
    /// `invalidate_split_regions` primitive to correlate cache entries
    /// (keyed by `MachineInsnAddr`) with regions in a freshly rebuilt
    /// CFG.  Distinct from [`Self::predecessor_count`] / region lookup
    /// by id: this is a content-keyed lookup that is stable across CFG
    /// rebuilds (same machine address always produces the same key).
    ///
    /// Also used by the **`opt` crate's** `IndirectBranchResolve`
    /// pass when resolving an indirect branch's anchor to the
    /// region that owns it.  The method is `pub` (not `pub(crate)`
    /// / `test_api`) precisely so that cross-crate consumers —
    /// `opt`, `strider`, future analysis crates — can correlate
    /// machine addresses with regions without going through a
    /// private channel.
    ///
    /// CORRECTNESS: only matches regions whose `start_addr.machine_addr`
    /// equals `addr` exactly.  Mid-region matches return `None` — the
    /// caller is interested in the canonical region whose lift would
    /// populate the cache entry, which is the region that *starts* at
    /// `addr`.  After a `split_region` event, the second-half region's
    /// start is a different machine address (the split point), so this
    /// lookup transparently distinguishes pre- and post-split halves.
    #[must_use]
    pub fn region_id_at_start(&self, addr: super::types::MachineInsnAddr) -> Option<RegionId> {
        for rid in self.graph.node_indices() {
            if let Some(region) = self.graph.node_weight(rid)
                && region.start_addr.machine_addr == addr
            {
                return Some(rid);
            }
        }
        None
    }
}

