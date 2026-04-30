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
    /// Returns an error when more than one outgoing edge of `kind` is
    /// attached to `region_id`.
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
    /// Returns an error when more than one `Branch` edge leaves
    /// `region_id`.
    pub fn region_branch(&self, region_id: RegionId) -> Result<Option<NodeIndex>> {
        self.unique_outgoing(region_id, RegionEdgeKind::Branch)
    }

    /// Returns the fallthrough successor of `region_id`, if any.
    ///
    /// A region's fallthrough edge is its successor on the
    /// `Fallthrough` edge kind — emitted either by sequential decode
    /// reaching a known region OR by the builder reclassifying a
    /// `Branch` whose target was the next machine instruction.
    ///
    /// # Errors
    /// Returns an error when more than one `Fallthrough` edge leaves
    /// `region_id`.
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

    /// Returns the `RegionId` of the region whose **start machine
    /// address** equals `addr`, or `None` if no such region exists.
    ///
    /// Content-keyed lookup that is stable across CFG rebuilds (same
    /// machine address always produces the same key).  Used by the
    /// `opt` crate's `IndirectBranchResolve` pass and by `strider`'s
    /// switch handler to correlate a machine address with the region
    /// that owns it.
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

